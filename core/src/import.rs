//! Importing a mod made without this tool (a download from a mod site), as far as can be done
//! safely, so that it installs and uninstalls beside other mods. This is a best effort for the mods
//! that exist already, not a format to keep up: a mod shared with `mod dist` needs none of it.
//!
//! The zip is never extracted as it is. Each file is read within limits, checked, and placed by
//! these rules:
//! - a UE4SS mod's folder (one holding `Scripts/` or `dlls/main.dll`, wherever it is in the zip)
//!   goes to `ue4ss/Mods/<its name>/`: its Lua, `dlls/main.dll`, `enabled.txt` and text settings;
//!   a shared Lua module (`Mods/shared/<name>/`) to `ue4ss/Mods/shared/<name>/`;
//! - containers (`.pak`, `.utoc`, `.ucas`) go to `Paks/~mods/`, or `Paks/LogicMods/` if they're
//!   in a `LogicMods` folder in the zip;
//! - documents and pictures are left out; so are UE4SS's own files (a bundled UE4SS, its
//!   settings, signatures, `mods.txt`, a `.usmap`), with a caution: those change UE4SS for every
//!   mod, so they're for installing by hand, if at all.
//!
//! It refuses the whole zip for: an unsafe path (`..`, a drive, a device name, a stream), a link,
//! encryption, a program or script file, an archive inside it, a DLL other than a UE4SS mod's
//! `dlls/main.dll` (another would be loaded some other way, as beside the game's executable), a
//! file where one already is in the game folder (it never unzips over anything), or too much data
//! (a zip bomb).
//!
//! A container that only replaces game DataTables is turned into table edits, so that its changes
//! merge with other mods' instead of replacing whole tables. The edits must give back exactly the
//! mod's tables, or the container is kept as it is. What can't be checked (a C++ mod's detours, a
//! container's whole replacements, Lua that reaches beyond the game) is recorded as cautions,
//! warned about whenever the mod is installed.
//!
//! The result is a zip in the first mods folder, read as any mod's zip is (see
//! [`crate::mods::Archive`]): tick it to install it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::datatable::read_datatable;
use crate::files::{FileRecord, GameFiles, Manifest};
use crate::game::display_path;
use crate::mods::{
    function_paths, game_build, is_name, string_arguments, write_mod_zip, Entry, Workspace,
    PAK_FOLDER, UE4SS_FOLDER, UE4SS_OWN,
};
use crate::report::{self, note};

/// Limits on what's read from a zip.
const MAX_FILES: usize = 5_000;
const MAX_TOTAL: u64 = 1 << 30;
/// Uncompressed to compressed, for files over a megabyte: more is a zip bomb.
const MAX_RATIO: u64 = 100;
const MAX_PART: usize = 120;
const MAX_PATH: usize = 240;

/// Extensions that are programs or scripts outside the game's Lua: no mod needs them installed.
const PROGRAMS: &[&str] = &[
    "exe", "bat", "cmd", "com", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi",
    "msp", "scr", "reg", "lnk", "url", "hta", "cpl", "jar", "py", "pyw", "sh", "inf", "sys", "drv",
    "ocx", "so", "dylib",
];
const ARCHIVES: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso",
];
const CONTAINERS: &[&str] = &["pak", "utoc", "ucas"];
/// Files installed with a UE4SS mod besides its Lua and DLL: settings and plain text.
const SETTINGS: &[&str] = &["txt", "ini", "json", "cfg", "toml", "csv", "yaml", "yml"];
/// UE4SS's own files, left out: they change UE4SS for every mod.
const UE4SS_FILES: &[&str] = &[
    "ue4ss.dll",
    "ue4ss-settings.ini",
    "dwmapi.dll",
    "xinput1_3.dll",
    "mods.txt",
    "mods.json",
];
/// The tool's own containers (see `mods.rs`), which no import may name.
const RESERVED_CONTAINERS: &[&str] = &["zzz_ModTables_P", "zzz_ModSave_P"];
/// What in a Lua script reaches beyond the game, and what it does.
const LUA_REACH: &[(&str, &str)] = &[
    ("os.execute", "runs programs"),
    ("io.popen", "runs programs"),
    ("os.remove", "deletes files"),
    ("os.rename", "moves files"),
    ("io.open", "opens files"),
    ("io.lines", "reads files"),
    ("package.loadlib", "loads native code"),
    ("loadstring", "runs code made from text"),
    ("require(\"socket", "uses the network"),
    ("require \"socket", "uses the network"),
    ("ffi.", "calls native code"),
    ("debug.", "uses the debug library"),
];
/// What a DLL that names these does beyond detouring the game's code.
const DLL_REACH: &[(&str, &str)] = &[
    ("URLDownloadToFile", "downloads files"),
    ("InternetOpen", "uses the internet"),
    ("WinHttpOpen", "uses the internet"),
    ("WSAStartup", "uses the network"),
    ("CreateProcess", "runs programs"),
    ("ShellExecute", "runs programs"),
    ("WinExec", "runs programs"),
    ("RegSetValue", "changes the registry"),
    ("RegCreateKey", "changes the registry"),
];

/// A file read from the zip: its path's parts, checked, and its contents.
struct ZipEntry {
    path: Vec<String>,
    contents: Vec<u8>,
}

impl ZipEntry {
    fn text(&self) -> String {
        self.path.join("/")
    }
    fn extension(&self) -> String {
        extension(self.path.last().map(String::as_str).unwrap_or_default())
    }
}

fn extension(file: &str) -> String {
    file.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default()
}

fn same(part: &str, name: &str) -> bool {
    part.eq_ignore_ascii_case(name)
}

/// The parts of a path in a zip, if every one is a plain name Windows reads as it's written: no
/// `.` or `..`, no drive or stream (`:`), no device name (`NUL`, `COM1`), no characters Windows
/// forbids, no trailing dot or space. Zips made by some Windows tools separate folders with `\`,
/// which is read as `/`.
fn plain_parts(name: &str) -> Result<Vec<String>> {
    let unsafe_path = || anyhow::anyhow!("{name:?} isn't a plain path inside the zip");
    let name = name.replace('\\', "/");
    if name.starts_with('/') || name.len() > MAX_PATH {
        return Err(unsafe_path());
    }
    let parts: Vec<String> = name
        .trim_end_matches('/')
        .split('/')
        .map(str::to_string)
        .collect();
    for part in &parts {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > MAX_PART
            || part.ends_with('.')
            || part.ends_with(' ')
            || part
                .chars()
                .any(|character| character.is_control() || "<>:\"|?*".contains(character))
            || device;
        if bad {
            return Err(unsafe_path());
        }
    }
    Ok(parts)
}

/// Every file in the zip, checked (see the module's comment) and read within the limits.
fn read_zip(path: &Path) -> Result<Vec<ZipEntry>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("opening {}", display_path(path)))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} isn't a zip", display_path(path)))?;
    if zip.len() > MAX_FILES {
        bail!("it holds more than {MAX_FILES} files");
    }
    let mut entries = vec![];
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index)?;
        let name = file.name().to_string();
        if file.encrypted() {
            bail!("{name} is encrypted");
        }
        if file.is_symlink() {
            bail!("{name} is a link, which could lead anywhere");
        }
        // (with `\` as a separator, as plain_parts reads it)
        if file.enclosed_name().is_none() && !name.contains('\\') {
            bail!("{name:?} isn't a plain path inside the zip");
        }
        let parts = plain_parts(&name)?;
        // Windows doesn't tell names apart by case
        if !seen.insert(parts.join("/").to_lowercase()) {
            bail!("{name} is in it twice");
        }
        if file.is_dir() {
            continue;
        }
        let size = file.size();
        if size > 1 << 20 && size / file.compressed_size().max(1) > MAX_RATIO {
            bail!("{name} expands too much for its size (a zip bomb?)");
        }
        total += size;
        if total > MAX_TOTAL {
            bail!("it holds more than {} MB", MAX_TOTAL >> 20);
        }
        let mut contents = Vec::with_capacity(size.min(64 << 20) as usize);
        (&mut file).take(size + 1).read_to_end(&mut contents)?;
        if contents.len() as u64 != size {
            bail!("{name} isn't the size the zip says");
        }
        entries.push(ZipEntry {
            path: parts,
            contents,
        });
    }
    Ok(entries)
}

/// Where each file of the zip goes, and why the rest don't.
#[derive(Default, Debug)]
struct Plan {
    /// Destination (relative to the game folder) -> the file's index in the zip.
    install: BTreeMap<String, usize>,
    /// The UE4SS mod folders it makes, below `ue4ss/Mods/`.
    ue4ss_folders: BTreeSet<String>,
    /// The containers it installs, as destinations without their extensions.
    containers: BTreeSet<String>,
    left_out: Vec<String>,
    cautions: Vec<String>,
    /// Why the zip can't be imported at all.
    refused: Vec<String>,
}

/// A UE4SS mod folder's name, if it's one this tool would install: plain, and not UE4SS's own.
fn ue4ss_folder_name(name: &str) -> Result<(), String> {
    let plain = name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_-. ".contains(character));
    if !plain || name.trim().is_empty() || same(name, "shared") {
        return Err(format!(
            "{name:?} isn't a name a UE4SS mod's folder can have here"
        ));
    }
    if UE4SS_OWN.iter().any(|own| same(own, name)) {
        return Err(format!(
            "its {name} folder would replace one of UE4SS's own mods"
        ));
    }
    Ok(())
}

/// Place each file by the rules in the module's comment. `fallback` names a UE4SS mod folder at the
/// zip's top.
fn plan(entries: &[ZipEntry], fallback: &str) -> Plan {
    let mut plan = Plan::default();
    let ue4ss = UE4SS_FOLDER.join("/");
    let paks = PAK_FOLDER[..3].join("/");
    // the folders directly holding a `Scripts` folder with Lua, or `dlls/main.dll`
    let mut mod_folders: BTreeSet<Vec<String>> = BTreeSet::new();
    for entry in entries {
        let path = &entry.path;
        for (at, part) in path.iter().enumerate().take(path.len().saturating_sub(1)) {
            let scripts = same(part, "Scripts") && entry.extension() == "lua";
            let dll = same(part, "dlls") && at + 2 == path.len() && same(&path[at + 1], "main.dll");
            if scripts || dll {
                mod_folders.insert(path[..at].to_vec());
                break;
            }
        }
    }
    let mut ue4ss_parts: BTreeMap<String, String> = BTreeMap::new(); // UE4SS's own files -> why
    for (index, entry) in entries.iter().enumerate() {
        let path = &entry.path;
        let file = path.last().cloned().unwrap_or_default();
        let kind = entry.extension();
        if PROGRAMS.contains(&kind.as_str()) {
            plan.refused.push(format!(
                "{} is a program or script file, which no mod needs",
                entry.text()
            ));
            continue;
        }
        if ARCHIVES.contains(&kind.as_str()) {
            plan.refused.push(format!(
                "{} is an archive inside the zip: unpack it and import that",
                entry.text()
            ));
            continue;
        }
        // a shared Lua module: .../Mods/shared/<name>/...
        let shared = path
            .windows(2)
            .position(|pair| same(&pair[0], "Mods") && same(&pair[1], "shared"));
        if let Some(at) = shared.filter(|at| path.len() > at + 3) {
            let module = &path[at + 2];
            match ue4ss_folder_name(module) {
                Ok(()) if kind == "lua" || SETTINGS.contains(&kind.as_str()) => {
                    let destination = format!("{ue4ss}/shared/{}", path[at + 2..].join("/"));
                    plan.ue4ss_folders.insert(format!("shared/{module}"));
                    add(&mut plan, destination, index, entry);
                }
                Ok(()) => plan.left_out.push(entry.text()),
                Err(why) => plan.refused.push(why),
            }
            continue;
        }
        // in a UE4SS mod's folder (the outermost one holding it)
        let owner = mod_folders
            .iter()
            .filter(|folder| path.starts_with(folder))
            .min_by_key(|folder| folder.len());
        if let Some(folder) = owner {
            let name = folder
                .last()
                .cloned()
                .unwrap_or_else(|| fallback.to_string());
            if let Err(why) = ue4ss_folder_name(&name) {
                plan.refused.push(why);
                continue;
            }
            let inside = &path[folder.len()..];
            let is_main_dll =
                inside.len() == 2 && same(&inside[0], "dlls") && same(&inside[1], "main.dll");
            if kind == "dll" && !is_main_dll {
                plan.refused.push(format!(
                    "{} is a DLL other than dlls/main.dll, which UE4SS wouldn't load as the mod",
                    entry.text()
                ));
                continue;
            }
            if kind == "lua" || is_main_dll || SETTINGS.contains(&kind.as_str()) {
                plan.ue4ss_folders.insert(name.clone());
                add(
                    &mut plan,
                    format!("{ue4ss}/{name}/{}", inside.join("/")),
                    index,
                    entry,
                );
            } else {
                plan.left_out.push(entry.text());
            }
            continue;
        }
        if CONTAINERS.contains(&kind.as_str()) {
            let stem = file
                .rsplit_once('.')
                .map_or(file.as_str(), |(stem, _)| stem);
            if RESERVED_CONTAINERS
                .iter()
                .any(|reserved| same(reserved, stem))
            {
                plan.refused.push(format!(
                    "{} has the name of this tool's merged container",
                    entry.text()
                ));
                continue;
            }
            let folder = if path.iter().any(|part| same(part, "LogicMods")) {
                "LogicMods"
            } else {
                "~mods"
            };
            plan.containers.insert(format!("{paks}/{folder}/{stem}"));
            add(&mut plan, format!("{paks}/{folder}/{file}"), index, entry);
            continue;
        }
        let is_ue4ss_own = UE4SS_FILES.iter().any(|own| same(own, &file))
            || path.iter().any(|part| same(part, "UE4SS_Signatures"))
            || kind == "usmap";
        if is_ue4ss_own {
            ue4ss_parts.insert(entry.text(), "UE4SS's own".to_string());
            continue;
        }
        if kind == "dll" {
            plan.refused.push(format!(
                "{} is a DLL outside a UE4SS mod's folder: something other than UE4SS would load \
                 it (beside the game's executable, say)",
                entry.text()
            ));
            continue;
        }
        if kind == "lua" {
            plan.cautions.push(format!(
                "left out {}, a Lua script outside a UE4SS mod's folder, which UE4SS wouldn't run",
                entry.text()
            ));
        }
        plan.left_out.push(entry.text());
    }
    if !ue4ss_parts.is_empty() {
        let listed: Vec<&str> = ue4ss_parts.keys().map(String::as_str).collect();
        plan.cautions.push(format!(
            "left out UE4SS's own files ({}): they change UE4SS for every mod. If the mod needs \
             them, install them by hand, only if you trust it",
            listed.join(", ")
        ));
        plan.left_out.extend(ue4ss_parts.into_keys());
    }
    plan
}

fn add(plan: &mut Plan, destination: String, index: usize, entry: &ZipEntry) {
    if plan.install.insert(destination.clone(), index).is_some() {
        plan.refused.push(format!(
            "two of its files would both be {destination} ({})",
            entry.text()
        ));
    }
}

/// A mod name from a folder's or file's: its letters and digits, starting with a letter.
fn name_from(text: &str) -> String {
    let text = text.strip_prefix("zzz_").unwrap_or(text);
    let text = text.strip_suffix("_P").unwrap_or(text);
    let kept: String = text
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .skip_while(|character| character.is_ascii_digit())
        .collect();
    if kept.is_empty() {
        "Imported".to_string()
    } else {
        kept
    }
}

/// A version from a file name, as `v0.1.7` or `1.2` in it; else 1.0.0.
fn version_from(text: &str) -> String {
    let pattern =
        regex::Regex::new(r"(?:^|[^0-9.])v?(\d{1,4})\.(\d{1,4})(?:\.(\d{1,4}))?").unwrap();
    pattern
        .captures(text)
        .map(|found| {
            let part = |index: usize| {
                found
                    .get(index)
                    .and_then(|part| part.as_str().parse::<u16>().ok())
                    .unwrap_or(0)
            };
            format!("{}.{}.{}", part(1), part(2), part(3))
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

/// What in a script or DLL reaches beyond the game: the matches' descriptions.
fn reaches(contents: &[u8], patterns: &[(&str, &str)]) -> Vec<String> {
    let text = String::from_utf8_lossy(contents);
    let found: BTreeSet<String> = patterns
        .iter()
        .filter(|(pattern, _)| text.contains(pattern))
        .map(|(pattern, what)| format!("{pattern} ({what})"))
        .collect();
    found.into_iter().collect()
}

/// Every difference of `modded` from `game`, as an edit: objects field by field, anything else
/// whole. None if they're the same.
fn difference(game: &Value, modded: &Value) -> Option<Value> {
    match (game, modded) {
        (Value::Object(game), Value::Object(modded)) => {
            let mut changed = Map::new();
            for (key, value) in modded {
                match game.get(key) {
                    Some(original) => {
                        if let Some(edit) = difference(original, value) {
                            changed.insert(key.clone(), edit);
                        }
                    }
                    None => {
                        changed.insert(key.clone(), value.clone());
                    }
                }
            }
            (!changed.is_empty()).then_some(Value::Object(changed))
        }
        _ => (game != modded).then(|| modded.clone()),
    }
}

/// What importing a zip made.
#[derive(Debug)]
pub struct Imported {
    pub name: String,
    /// The mod's zip in the first mods folder.
    pub path: PathBuf,
    pub installs: Vec<String>,
    pub left_out: Vec<String>,
    pub cautions: Vec<String>,
}

impl Workspace {
    /// Import a mod made without this tool (see the module's comment), as a zip in the first mods
    /// folder. `name` and `version` replace what the zip's names suggest.
    pub fn import(
        &self,
        zip: &Path,
        name: Option<&str>,
        version: Option<&str>,
    ) -> Result<Imported> {
        let stem = zip
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entries =
            read_zip(zip).with_context(|| format!("can't import {}", display_path(zip)))?;
        let fallback = name.map(str::to_string).unwrap_or_else(|| name_from(&stem));
        let mut plan = plan(&entries, &fallback);
        let name = match name {
            Some(name) => name.to_string(),
            None => match (
                plan.ue4ss_folders.iter().next(),
                plan.containers.iter().next(),
            ) {
                (Some(folder), _) => name_from(folder.rsplit('/').next().unwrap_or(folder)),
                (None, Some(container)) => {
                    name_from(container.rsplit('/').next().unwrap_or(container))
                }
                _ => fallback,
            },
        };
        if !is_name(&name) {
            bail!("{name:?} isn't a mod name: give one with letters and digits, starting with a letter");
        }
        let version = version
            .map(str::to_string)
            .unwrap_or_else(|| version_from(&stem));
        crate::mods::parse_version(&version, &name)?;
        if plan.install.is_empty() && plan.refused.is_empty() {
            bail!(
                "{} holds nothing this tool installs (no UE4SS mod folder or container); it left \
                 out: {}",
                display_path(zip),
                plan.left_out.join(", ")
            );
        }
        let record = self.record()?;
        let taken = self.local_folder(&name).is_some()
            || self.archives().contains_key(&name)
            || record.mods.contains_key(&name);
        if taken {
            bail!("there's a mod named {name} already: give the import another name");
        }
        // it never unzips over anything
        let files = GameFiles::new(&self.game.root)?;
        for destination in plan.install.keys() {
            if files.resolve(destination)?.exists() {
                plan.refused
                    .push(format!("{destination} is in the game folder already"));
            }
        }
        for folder in &plan.ue4ss_folders {
            let relative = format!("{}/{folder}", UE4SS_FOLDER.join("/"));
            if files.resolve(&relative)?.exists() {
                plan.refused.push(format!(
                    "its UE4SS folder {relative} is in the game folder already"
                ));
            }
        }
        if !plan.refused.is_empty() {
            plan.refused.sort();
            plan.refused.dedup();
            bail!(
                "can't import {}:\n   {}",
                display_path(zip),
                plan.refused.join("\n   ")
            );
        }
        let Some(mods) = self.mods.first() else {
            bail!("no mods folder is set to import into: set one first");
        };
        let path = mods.join(format!("{name}_{version}.zip"));
        if path.exists() {
            bail!("{} exists already", display_path(&path));
        }

        let mut entry = Entry {
            name: name.clone(),
            title: name.clone(),
            version: version.clone(),
            ..Entry::default()
        };
        let mut cautions = plan.cautions.clone();
        // what its code can do, as far as can be seen
        let mut hooks = BTreeSet::new();
        for (destination, &index) in &plan.install {
            let contents = &entries[index].contents;
            if destination.ends_with(".lua") {
                hooks.extend(string_arguments(
                    &String::from_utf8_lossy(contents),
                    "RegisterHook",
                ));
                let found = reaches(contents, LUA_REACH);
                if !found.is_empty() {
                    cautions.push(format!(
                        "its Lua {} uses {}: check what it does with them",
                        destination.rsplit('/').next().unwrap_or(destination),
                        found.join(", ")
                    ));
                }
            }
            if destination.to_ascii_lowercase().ends_with("/dlls/main.dll") {
                hooks.extend(function_paths(contents));
                cautions.push(
                    "its C++ (dlls/main.dll) runs with full access to your PC and can change any \
                     of the game's code; what it detours can't be seen, so a clash with another \
                     mod's C++ can't be ruled out"
                        .to_string(),
                );
                let found = reaches(contents, DLL_REACH);
                if !found.is_empty() {
                    cautions.push(format!("its DLL names {}", found.join(", ")));
                }
            }
        }
        entry.hooks = hooks.into_iter().collect();
        entry.needs_ue4ss = !plan.ue4ss_folders.is_empty();

        // containers: tables become edits where that gives exactly the mod's tables
        let mut dropped = HashSet::new();
        for container in plan.containers.clone() {
            match self.import_container(&plan, &entries, &container, &name) {
                Ok(Converted::Edits { edits, tables }) => {
                    note!(
                        "   {}: its tables ({}) are now table edits, merged with other mods'",
                        container.rsplit('/').next().unwrap_or(&container),
                        tables.join(", ")
                    );
                    cautions.push(format!(
                        "its changes to {} were worked out against the game as it was when it \
                         was imported (build {}): if it was made for an older build, they may \
                         undo changes the update made",
                        tables.join(", "),
                        game_build(&self.game).unwrap_or_else(|| "unknown".to_string())
                    ));
                    entry.edits.extend(edits);
                    entry.tables.extend(tables);
                    dropped.insert(container);
                }
                Ok(Converted::Kept(caution)) => cautions.extend(caution),
                Err(error) => cautions.push(format!(
                    "{} is kept as it is: {error:#}",
                    container.rsplit('/').next().unwrap_or(&container)
                )),
            }
        }
        // a UE4SS mod loads when its folder has enabled.txt
        let mut added = vec![];
        for folder in plan
            .ue4ss_folders
            .iter()
            .filter(|folder| !folder.starts_with("shared/"))
        {
            let enabled = format!("{}/{folder}/enabled.txt", UE4SS_FOLDER.join("/"));
            if !plan
                .install
                .keys()
                .any(|destination| destination.eq_ignore_ascii_case(&enabled))
            {
                added.push((enabled, vec![]));
            }
        }
        let mut contents: Vec<(String, Vec<u8>)> = plan
            .install
            .iter()
            .filter(|(destination, _)| {
                let stem = destination
                    .rsplit_once('.')
                    .map_or(destination.as_str(), |(stem, _)| stem);
                !dropped.contains(stem)
            })
            .map(|(destination, &index)| (destination.clone(), entries[index].contents.clone()))
            .collect();
        contents.extend(added);
        let records: Vec<FileRecord> = contents
            .iter()
            .map(|(relative, bytes)| FileRecord {
                path: relative.clone(),
                size: bytes.len() as u64,
                sha256: Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            })
            .collect();
        entry.files = records.iter().map(|record| record.path.clone()).collect();
        cautions.insert(
            0,
            format!(
                "imported by best effort from {}, a mod made without this tool",
                zip.file_name()
                    .map(|file| file.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
        );
        entry.cautions = cautions.clone();
        let manifest = Manifest {
            installed_by: "import".to_string(),
            files: records,
            ..Self::manifest_for(&entry, "import", vec![])
        };
        std::fs::create_dir_all(mods)?;
        write_mod_zip(&path, &manifest, &entry, contents)?;
        for caution in &cautions {
            report::warning_about(std::slice::from_ref(&name), caution.clone());
        }
        note!(
            "imported {name} {version} -> {}: tick it to install it",
            display_path(&path)
        );
        Ok(Imported {
            name,
            path,
            installs: entry.files,
            left_out: plan.left_out,
            cautions,
        })
    }

    /// Turn a container's tables into edits, if it holds only game DataTables and the edits give
    /// back exactly its tables; else say what keeping it means.
    fn import_container(
        &self,
        plan: &Plan,
        entries: &[ZipEntry],
        container: &str,
        name: &str,
    ) -> Result<Converted> {
        let file = container.rsplit('/').next().unwrap_or(container);
        let part = |extension: &str| {
            plan.install
                .get(&format!("{container}.{extension}"))
                .map(|&index| &entries[index].contents)
        };
        let Some(utoc) = part("utoc") else {
            return Ok(Converted::Kept(vec![format!(
                "{file} is a .pak without .utoc and .ucas: this game (Unreal Engine 5) may not \
                 load it, and what it holds can't be checked"
            )]));
        };
        let (Some(ucas), Some(pak)) = (part("ucas"), part("pak")) else {
            bail!("its .utoc needs a .ucas and a .pak beside it");
        };
        // copied to a name the game's own containers give way to, as installed mods' do
        let work = self.build.join(".import").join(name);
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let copy = work.join("zzz_Import_P.utoc");
        std::fs::write(&copy, utoc)?;
        std::fs::write(copy.with_extension("ucas"), ucas)?;
        std::fs::write(copy.with_extension("pak"), pak)?;
        let result = self.convert_tables(&copy, file);
        let _ = std::fs::remove_dir_all(&work);
        result
    }

    fn convert_tables(&self, copy: &Path, file: &str) -> Result<Converted> {
        let packages = crate::unpack::package_names(copy).context("reading what it holds")?;
        let game_packages =
            crate::unpack::package_names(&self.game.paks().join("AlchemyFactory-Windows.utoc"))?;
        let mut replaced: Vec<&String> = packages
            .iter()
            .filter(|package| game_packages.contains(*package))
            .collect();
        replaced.sort();
        if replaced.is_empty() {
            // only assets of its own, which no other mod's can be
            return Ok(Converted::Kept(vec![]));
        }
        let shown = |list: &[&String]| {
            let names: Vec<&str> = list.iter().take(6).map(|name| name.as_str()).collect();
            format!(
                "{}{}",
                names.join(", "),
                if list.len() > 6 { ", ..." } else { "" }
            )
        };
        let whole = |why: &str| {
            Converted::Kept(vec![format!(
                "{file} replaces these game assets whole ({why}): {}. Only one copy of each \
                 takes effect, so it may undo, or be undone by, another mod's",
                shown(&replaced)
            )])
        };
        let paths: Vec<String> = replaced
            .iter()
            .map(|package| format!("AlchemyFactory/Content/{}", &package["/Game/".len()..]))
            .collect();
        let (assets, _) = crate::package::convert_back(copy, &paths, &self.game)?;
        let all_tables = paths
            .iter()
            .all(|path| assets.class(path) == Some("DataTable"));
        if !all_tables || packages.len() != replaced.len() {
            return Ok(whole("not only DataTables"));
        }
        let game = self.game_for_edits()?;
        let schemas = self.schemas()?;
        let mut edits = Map::new();
        let mut decoded = BTreeMap::new();
        for path in &paths {
            let table = path.rsplit('/').next().unwrap_or(path).to_string();
            let modded = read_datatable(assets.asset(path)?, schemas, None)?;
            let original = read_datatable(game.assets.asset(path)?, schemas, None)?;
            let (Some(modded_rows), Some(original_rows)) =
                (modded["rows"].as_object(), original["rows"].as_object())
            else {
                return Ok(whole("tables without rows"));
            };
            if original_rows
                .keys()
                .any(|row| !modded_rows.contains_key(row))
            {
                return Ok(whole("it removes rows, which edits can't"));
            }
            let first = original_rows.keys().next().cloned().unwrap_or_default();
            let mut rows = Map::new();
            for (row, fields) in modded_rows {
                match original_rows.get(row) {
                    Some(original) => {
                        if let Some(changed) = difference(original, fields) {
                            rows.insert(row.clone(), changed);
                        }
                    }
                    None => {
                        // a new row starts as a copy of another, then gets every field
                        let mut new = fields.as_object().cloned().unwrap_or_default();
                        new.insert("_copy".to_string(), json!(first));
                        rows.insert(row.clone(), Value::Object(new));
                    }
                }
            }
            if !rows.is_empty() {
                edits.insert(table.clone(), Value::Object(rows));
            }
            decoded.insert(table, (path.clone(), modded["rows"].clone()));
        }
        if edits.is_empty() {
            return Ok(whole(
                "its tables are as the game's, so it changes nothing now",
            ));
        }
        let label = format!("imported/{file}.json");
        let built = crate::edits::build(
            &game,
            &[crate::edits::Source {
                label: label.clone(),
                contents: Value::Object(edits.clone()),
            }],
        );
        let built = match built {
            Ok(built) => built,
            Err(error) => {
                return Ok(whole(&format!(
                    "its tables don't make valid edits: {error:#}"
                )))
            }
        };
        // the edits must give back exactly the mod's tables
        for (table, (path, rows)) in &decoded {
            let Some(asset) = built.tables.iter().find(|asset| asset.path == *path) else {
                if edits.contains_key(table) {
                    return Ok(whole("the edits didn't change its tables"));
                }
                continue;
            };
            let back = read_datatable((&asset.uasset, &asset.uexp), schemas, None)?;
            if back["rows"] != *rows {
                return Ok(whole("edits don't give exactly its tables"));
            }
        }
        Ok(Converted::Edits {
            edits: vec![(label, Value::Object(edits))],
            tables: decoded.into_keys().collect(),
        })
    }
}

/// What became of a container.
enum Converted {
    /// Its tables, as edits (one file's), and their names.
    Edits {
        edits: Vec<(String, Value)>,
        tables: Vec<String>,
    },
    /// Kept as it is, with these cautions.
    Kept(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_entries(paths: &[&str]) -> Vec<ZipEntry> {
        paths
            .iter()
            .map(|path| ZipEntry {
                path: plain_parts(path).unwrap(),
                contents: b"print(1)".to_vec(),
            })
            .collect()
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in [
            "../x.lua",
            "a/../../x",
            "/x",
            "C:/x",
            "..\\x",
            "a\\..\\..\\b",
            "C:\\x",
            "a/./b",
            "a/NUL.txt",
            "a/com1",
            "a/file.txt:stream",
            "a/b. ",
            "a/b.",
        ] {
            assert!(plain_parts(bad).is_err(), "{bad} was allowed");
        }
        assert_eq!(plain_parts("A/b c/d.lua").unwrap(), ["A", "b c", "d.lua"]);
        assert_eq!(
            plain_parts("Mods\\A\\enabled.txt").unwrap(),
            ["Mods", "A", "enabled.txt"]
        );
    }

    #[test]
    fn layouts_are_placed() {
        let ue4ss = UE4SS_FOLDER.join("/");
        // a mod folder at the top, with documents
        let plan = plan(
            &zip_entries(&[
                "FountainControl/Scripts/main.lua",
                "FountainControl/dlls/main.dll",
                "FountainControl/enabled.txt",
                "FountainControl/README.md",
                "FountainControl/docs/images/a.png",
            ]),
            "Fallback",
        );
        assert!(plan.refused.is_empty(), "{:?}", plan.refused);
        assert!(plan
            .install
            .contains_key(&format!("{ue4ss}/FountainControl/dlls/main.dll")));
        assert_eq!(plan.left_out.len(), 2);
        // the game folder's layout under another name, with UE4SS's own files beside a mod
        let plan = super::plan(
            &zip_entries(&[
                "X/AlchemyFactory.1.0/Binaries/Win64/ue4ss/Mods/Probe/scripts/main.lua",
                "Mods/Other/Scripts/main.lua",
                "UE4SS_Signatures/GNatives.lua",
                "Mappings.usmap",
                "Paks/zzz_Thing_P.pak",
            ]),
            "Fallback",
        );
        assert!(plan.refused.is_empty(), "{:?}", plan.refused);
        assert!(plan
            .install
            .contains_key(&format!("{ue4ss}/Probe/scripts/main.lua")));
        assert!(plan
            .install
            .contains_key(&format!("{ue4ss}/Other/Scripts/main.lua")));
        assert!(plan
            .install
            .contains_key("AlchemyFactory/Content/Paks/~mods/zzz_Thing_P.pak"));
        assert!(plan
            .cautions
            .iter()
            .any(|caution| caution.contains("UE4SS's own")));
        // scripts at the top are a mod named for the zip
        let plan = super::plan(&zip_entries(&["Scripts/main.lua"]), "Fallback");
        assert!(plan
            .install
            .contains_key(&format!("{ue4ss}/Fallback/Scripts/main.lua")));
    }

    #[test]
    fn dangerous_zips_are_refused() {
        for (paths, why) in [
            (vec!["Mod/Scripts/main.lua", "setup.exe"], "program"),
            (vec!["Mod/Scripts/main.lua", "inner.zip"], "archive"),
            (
                vec!["Mod/Scripts/main.lua", "Mod/dlls/helper.dll"],
                "DLL other",
            ),
            (vec!["Binaries/Win64/version.dll"], "DLL outside"),
            (vec!["Keybinds/Scripts/main.lua"], "UE4SS's own mods"),
            (vec!["Paks/zzz_ModTables_P.pak"], "merged container"),
            (
                vec!["A/Mod/Scripts/main.lua", "B/Mod/Scripts/main.lua"],
                "both be",
            ),
        ] {
            let plan = plan(&zip_entries(&paths), "Fallback");
            assert!(
                plan.refused.iter().any(|reason| reason.contains(why)),
                "{paths:?}: {:?}",
                plan.refused
            );
        }
    }

    #[test]
    fn names_and_versions_from_files() {
        assert_eq!(name_from("zzz_BetterBelts_P"), "BetterBelts");
        assert_eq!(name_from("2 Fast Belts!"), "FastBelts");
        assert_eq!(
            version_from("FountainControl v0.1.7 33 v0.1.7 2026-10-07T21-33Z"),
            "0.1.7"
        );
        assert_eq!(
            version_from("AlchemyFactory UE4SS Config And Tools-5-1-1776545178"),
            "1.0.0"
        );
    }

    #[test]
    fn differences_are_edits() {
        let game = json!({"A": 1, "B": {"C": 2, "D": [1, 2]}});
        let modded = json!({"A": 1, "B": {"C": 3, "D": [1, 2]}});
        assert_eq!(difference(&game, &modded), Some(json!({"B": {"C": 3}})));
        assert_eq!(difference(&game, &game), None);
    }
}
