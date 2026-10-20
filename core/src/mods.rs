//! Mod folders: build, install and share mods kept as folders.
//!
//! A mod folder holds:
//! - `mod.json`: name, title, version, dependencies, cooked content to package, craft types;
//! - `edits/*.json`: table edits ([`crate::edits`]); `edits/if-<Mod>/`: applied only when
//!   `<Mod>` is installed too;
//! - `save/DefaultGame.json`: edits to the template save new games start from ([`crate::save`]);
//! - `ue4ss/`: a UE4SS mod, Lua (`Scripts/main.lua`) or C++ (`dlls/main.dll`) -> `ue4ss/Mods/<name>/`;
//! - `ue4ss-shared/<name>X/`: Lua modules other mods `require` -> `ue4ss/Mods/shared/<name>X/`;
//!
//! plus assets cooked in the stub project (`/Game/Mods/<name>/`), packed as `zzz_<name>Content_P`.
//!
//! The game reads a whole table from the last container that has it, so installed mods' table edits
//! are merged: every install and uninstall rebuilds one data container, `zzz_ModTables_P`, from the
//! edits of every installed mod in dependency order (and `zzz_ModSave_P` from their template-save
//! edits). The installed mods are recorded in `Paks/~mods/installed-mods.json`, in the same form as
//! records written by the mod project's retired Python `mods.py` are still read.
//!
//! `build/<name>/` mirrors the game folder. Its own data container (`zzz_<name>_P`: the mod and the
//! mods it requires) and template save are for the zip, for players without this tool.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::content;
use crate::datatable::read_datatable;
use crate::edits;
use crate::files::{hash, FileRecord, GameFiles, Manifest, State};
use crate::game::{display_path, join, Game};
use crate::package::{check_container, script_objects, write_container};
use crate::save::{self, SaveEdit};
use crate::schema::Schemas;
use crate::text::Localizer;
use crate::unpack::Assets;

const STEAM_APP: &str = "3669570";
const PAK_FOLDER: &[&str] = &["AlchemyFactory", "Content", "Paks", "~mods"];
const UE4SS_FOLDER: &[&str] = &["AlchemyFactory", "Binaries", "Win64", "ue4ss", "Mods"];
const RECORD: &str = "installed-mods.json";
/// The example edit file `mod new` writes, which applies nothing as it is.
const EXAMPLE_EDITS: &str = include_str!("../../templates/example-edits.json");
/// The merged data container: `zzz_ModTables_P`.
const MERGED: &str = "ModTables";
/// The merged template save: `zzz_ModSave_P` (see [`save_container`]).
const MERGED_SAVE: &str = "Mod";
const FIRST_CRAFT_TYPE: u8 = 26; // the game's craft types end at Max (25)
const MOD_FIELDS: &[&str] = &[
    "name",
    "title",
    "version",
    "description",
    "author",
    "contact",
    "homepage",
    "dependencies",
    "content",
    "craft-types",
    "new-craft-types",
];
const SAVE_EDIT_FIELDS: &[&str] = &["property", "value", "occurrence", "after", "expect", "in"];

// --- versions and dependencies -------------------------------------------------------------------

pub type Version = (u16, u16, u16);

pub fn parse_version(text: &str, at: &str) -> Result<Version> {
    let parts: Vec<&str> = text.split('.').collect();
    let numbers: Option<Vec<u16>> = parts.iter().map(|part| part.parse::<u16>().ok()).collect();
    match numbers.as_deref() {
        Some(&[a, b, c])
            if parts
                .iter()
                .all(|part| part.bytes().all(|byte| byte.is_ascii_digit())) =>
        {
            Ok((a, b, c))
        }
        _ => bail!("{at}: version {text:?} must be three numbers 0-65535, e.g. 0.1.0"),
    }
}

fn version_text(version: Version) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

/// How a mod depends on another (Factorio's dependency prefixes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// No prefix: must be installed, and loads first.
    Required,
    /// `?`: if installed, loads first, must match the version, and `edits/if-<Mod>/` apply.
    Optional,
    /// `(?)`: like `?`, but not listed in the README.
    HiddenOptional,
    /// `+`: mentioned when missing and listed in the README; changes nothing else.
    Recommended,
    /// `~`: required, but doesn't change the load order (for two mods that need each other).
    Unordered,
    /// `!`: neither installs while the other is installed.
    Incompatible,
}

impl Kind {
    fn is_required(self) -> bool {
        matches!(self, Kind::Required | Kind::Unordered)
    }
    fn affects_load_order(self) -> bool {
        matches!(self, Kind::Required | Kind::Optional | Kind::HiddenOptional)
    }
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub kind: Kind,
    pub name: String,
    pub operator: Option<String>,
    pub version: Option<Version>,
}

fn is_name(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

/// `"[prefix] Name [operator version]"`, e.g. `"VaultCore >= 0.2.0"` or `"? BiggerChests"`.
pub fn parse_dependency(text: &str, at: &str) -> Result<Dependency> {
    let invalid = || {
        anyhow::anyhow!(
            "{at}: dependency {text:?} is not \"[prefix] Name [operator version]\", e.g. \
             \"VaultCore >= 0.2.0\" or \"? BiggerChests\""
        )
    };
    let mut rest = text.trim();
    let kind = [
        ("(?)", Kind::HiddenOptional),
        ("!", Kind::Incompatible),
        ("?", Kind::Optional),
        ("+", Kind::Recommended),
        ("~", Kind::Unordered),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| rest.strip_prefix(prefix).map(|after| (kind, after)));
    let kind = match kind {
        Some((kind, after)) => {
            rest = after.trim_start();
            kind
        }
        None => Kind::Required,
    };
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    let name = &rest[..end];
    if !is_name(name) {
        return Err(invalid());
    }
    let rest = rest[end..].trim();
    let (operator, version) = if rest.is_empty() {
        (None, None)
    } else {
        let operator = ["<=", ">=", "<", ">", "="]
            .into_iter()
            .find(|candidate| rest.starts_with(candidate))
            .ok_or_else(invalid)?;
        let version = rest[operator.len()..].trim();
        (
            Some(operator.to_string()),
            Some(parse_version(version, at)?),
        )
    };
    Ok(Dependency {
        kind,
        name: name.to_string(),
        operator,
        version,
    })
}

fn satisfies(version: Version, dependency: &Dependency) -> bool {
    match (dependency.operator.as_deref(), dependency.version) {
        (Some("<"), Some(wanted)) => version < wanted,
        (Some("<="), Some(wanted)) => version <= wanted,
        (Some("="), Some(wanted)) => version == wanted,
        (Some(">="), Some(wanted)) => version >= wanted,
        (Some(">"), Some(wanted)) => version > wanted,
        _ => true,
    }
}

/// A dependency's version requirement, for messages.
fn requirement_text(dependency: &Dependency) -> String {
    match (&dependency.operator, dependency.version) {
        (Some(operator), Some(version)) => {
            format!("{} {operator} {}", dependency.name, version_text(version))
        }
        _ => dependency.name.clone(),
    }
}

// --- mods and installed entries ------------------------------------------------------------------

/// A mod as built and installed: the `build.json` of its build, and its entry in the install
/// record (the Python tool's form).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub version: String,
    pub game_build: Option<String>,
    pub dependencies: Vec<String>,
    /// Files it installs, relative to the game folder.
    pub files: Vec<String>,
    /// Its own data container and template save: for the zip only (installs merge them).
    pub data_files: Vec<String>,
    pub tables: Vec<String>,
    pub new_buildings: Vec<String>,
    pub needs_ue4ss: bool,
    /// Its new craft types: `Mod:Name` -> number.
    pub craft_type_numbers: IndexMap<String, u8>,
    /// Its edit files (craft type names already numbers): (label, contents).
    pub edits: Vec<(String, Value)>,
    pub optional_edits: IndexMap<String, Vec<(String, Value)>>,
    pub save_edits: Vec<SaveEdit>,
    /// The cooked assets it packs: object path -> class path.
    pub assets: IndexMap<String, String>,
    /// The rows it adds: (table, row).
    pub new_rows: Vec<(String, String)>,
}

impl Entry {
    fn requires(&self) -> Result<Vec<Dependency>> {
        self.dependencies
            .iter()
            .map(|text| parse_dependency(text, &self.name))
            .collect()
    }
    fn version(&self) -> Result<Version> {
        parse_version(&self.version, &self.name)
    }
}

/// A mod folder, read and checked.
pub struct Mod {
    pub folder: PathBuf,
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub contact: String,
    pub homepage: String,
    pub dependencies: Vec<String>,
    pub requires: Vec<Dependency>,
    pub content: Vec<String>,
    /// Blueprint asset -> craft type (a name, a number, or `Mod:Name`).
    pub craft_types: IndexMap<String, Value>,
    pub new_craft_types: Vec<String>,
    pub edits: Vec<(String, Value)>,
    pub optional_edits: IndexMap<String, Vec<(String, Value)>>,
    pub save_edits: Vec<SaveEdit>,
}

fn read_json(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", display_path(path)))?;
    serde_json::from_str(&text).with_context(|| format!("{} isn't valid JSON", display_path(path)))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("writing {}", display_path(path)))
}

/// `*.json` in a folder, sorted, as (label `<prefix>/<file>`, contents).
fn read_edits(folder: &Path, prefix: &str) -> Result<Vec<(String, Value)>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            Ok((
                format!("{prefix}/{}", path.file_name().unwrap().to_string_lossy()),
                read_json(&path)?,
            ))
        })
        .collect()
}

fn string_list(value: Option<&Value>, at: &str, field: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .with_context(|| format!("{at}: {field} lists strings"))
            })
            .collect(),
        Some(_) => bail!("{at}: {field} is a list"),
    }
}

impl Mod {
    pub fn load(folder: &Path) -> Result<Self> {
        let path = folder.join("mod.json");
        let at = display_path(&path);
        let raw = read_json(&path)?;
        let fields = raw
            .as_object()
            .with_context(|| format!("{at}: mod.json is an object"))?;
        let unknown: Vec<&String> = fields
            .keys()
            .filter(|key| !key.starts_with('_') && !MOD_FIELDS.contains(&key.as_str()))
            .collect();
        if !unknown.is_empty() {
            let unknown: Vec<&str> = unknown.iter().map(|key| key.as_str()).collect();
            let mut known = MOD_FIELDS.to_vec();
            known.sort();
            bail!(
                "{at}: unknown field(s) {} (fields: {})",
                unknown.join(", "),
                known.join(", ")
            );
        }
        let text = |key: &str, default: &str| {
            fields
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_string()
        };
        let name = text("name", "");
        if !is_name(&name) || name == MERGED || name == MERGED_SAVE {
            bail!(
                "{at}: name must be letters and digits, starting with a letter (it names files and \
                folders), \
                 and not {MERGED_SAVE} or {MERGED}"
            );
        }
        let version = text("version", "0.1.0");
        parse_version(&version, &at)?;
        let content = string_list(fields.get("content"), &at, "content")?;
        for path in &content {
            if !path.starts_with(&format!("/Game/Mods/{name}")) {
                bail!(
                    "{at}: content path {path} must be in /Game/Mods/{name}/, so that no two \
                    mods share asset paths"
                );
            }
        }
        let new_craft_types = string_list(fields.get("new-craft-types"), &at, "new-craft-types")?;
        if let Some(bad) = new_craft_types
            .iter()
            .find(|craft_type| !is_name(craft_type))
        {
            bail!("{at}: new craft type {bad:?} must be letters and digits");
        }
        let dependencies = string_list(fields.get("dependencies"), &at, "dependencies")?;
        let requires: Vec<Dependency> = dependencies
            .iter()
            .map(|text| parse_dependency(text, &at))
            .collect::<Result<_>>()?;
        let names: Vec<&str> = requires
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect();
        if names.contains(&name.as_str())
            || names.iter().collect::<HashSet<_>>().len() != names.len()
        {
            bail!("{at}: a mod can't depend on itself, or name the same mod twice");
        }
        let craft_types = fields
            .get("craft-types")
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let edits_folder = folder.join("edits");
        let edits = read_edits(&edits_folder, &name)?;
        let mut optional_edits = IndexMap::new();
        let mut subfolders: Vec<PathBuf> = std::fs::read_dir(&edits_folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|file_name| file_name.to_string_lossy().starts_with("if-"))
            })
            .collect();
        subfolders.sort();
        for sub in subfolders {
            let other = sub.file_name().unwrap().to_string_lossy()["if-".len()..].to_string();
            let dependency = requires.iter().find(|dependency| dependency.name == other);
            if dependency.is_none_or(|dependency| {
                dependency.kind.is_required() || dependency.kind == Kind::Incompatible
            }) {
                bail!(
                    "{}: edits for when {other} is installed need {other} as an optional \
                    dependency \
                     (\"? {other}\", \"(?) {other}\" or \"+ {other}\" in mod.json)",
                    display_path(&sub)
                );
            }
            optional_edits.insert(
                other.clone(),
                read_edits(&sub, &format!("{name}/if-{other}"))?,
            );
        }

        let mut save_edits = vec![];
        let saves = folder.join("save");
        if saves.is_dir() {
            for entry in std::fs::read_dir(&saves)?.flatten() {
                if entry.file_name() != "DefaultGame.json" {
                    bail!(
                        "{}: only DefaultGame.json (the template save new games start from) can be \
                        edited",
                        display_path(&entry.path())
                    );
                }
            }
            let file = saves.join("DefaultGame.json");
            let listed = read_json(&file)?;
            for edit in listed
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let object = edit.as_object().filter(|object| {
                    object
                        .keys()
                        .all(|key| key.starts_with('_') || SAVE_EDIT_FIELDS.contains(&key.as_str()))
                        && object.contains_key("property")
                        && object.contains_key("value")
                });
                let Some(object) = object else {
                    bail!(
                        "{}: each edit has \"property\" and \"value\", and optionally \
                        \"occurrence\", \
                         \"after\", \"expect\" and \"in\" (got {edit})",
                        display_path(&file)
                    );
                };
                let cleaned: serde_json::Map<String, Value> = object
                    .iter()
                    .filter(|(key, _)| !key.starts_with('_'))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                save_edits.push(serde_json::from_value(Value::Object(cleaned))?);
            }
        }
        Ok(Mod {
            folder: folder.to_path_buf(),
            title: text("title", &name),
            description: text("description", ""),
            author: text("author", ""),
            contact: text("contact", ""),
            homepage: text("homepage", ""),
            name,
            version,
            dependencies,
            requires,
            content,
            craft_types,
            new_craft_types,
            edits,
            optional_edits,
            save_edits,
        })
    }
}

fn data_container(name: &str) -> String {
    format!("zzz_{name}_P")
}

fn content_container(name: &str) -> String {
    format!("zzz_{name}Content_P")
}

fn save_container(name: &str) -> String {
    format!("zzz_{name}Save_P")
}

/// The changes Steam made last: the game's build number, from its app manifest.
pub fn game_build(game: &Game) -> Option<String> {
    let steamapps = game.root.parent()?.parent()?;
    let text =
        std::fs::read_to_string(steamapps.join(format!("appmanifest_{STEAM_APP}.acf"))).ok()?;
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("\"buildid\""))?;
    line.split('"').nth(3).map(str::to_string)
}

// --- the workspace: where mods, builds and the game are ------------------------------------------

/// Where things are, and what's read from the game once it's needed.
pub struct Workspace {
    pub mods: PathBuf,
    pub build: PathBuf,
    pub game: Game,
    pub usmap: PathBuf,
    pub project: PathBuf,
    schemas: OnceLock<Schemas>,
    assets: OnceLock<Assets>,
    localizer: OnceLock<Localizer>,
    packages: OnceLock<HashSet<String>>,
}

impl Workspace {
    pub fn new(
        mods: PathBuf,
        build: PathBuf,
        game: Game,
        usmap: PathBuf,
        project: PathBuf,
    ) -> Self {
        Workspace {
            mods,
            build,
            game,
            usmap,
            project,
            schemas: OnceLock::new(),
            assets: OnceLock::new(),
            localizer: OnceLock::new(),
            packages: OnceLock::new(),
        }
    }

    fn schemas(&self) -> Result<&Schemas> {
        if self.schemas.get().is_none() {
            let _ = self.schemas.set(crate::usmap::read_usmap(&self.usmap)?);
        }
        Ok(self.schemas.get().unwrap())
    }

    fn assets(&self) -> Result<&Assets> {
        if self.assets.get().is_none() {
            let _ = self.assets.set(crate::extract::content_assets(&self.game)?);
        }
        Ok(self.assets.get().unwrap())
    }

    fn game_for_edits(&self) -> Result<edits::Game<'_>> {
        let assets = self.assets()?;
        if self.localizer.get().is_none() {
            let _ = self
                .localizer
                .set(crate::extract::english(&self.game, assets)?);
        }
        if self.packages.get().is_none() {
            let _ = self
                .packages
                .set(crate::unpack::package_names(&self.game.paks())?);
        }
        Ok(edits::Game {
            assets,
            schemas: self.schemas()?,
            localizer: self.localizer.get(),
            packages: self.packages.get(),
        })
    }

    fn mod_folder(&self, name: &str) -> PathBuf {
        let given = Path::new(name);
        if given.join("mod.json").exists() {
            given.to_path_buf()
        } else {
            self.mods.join(name)
        }
    }

    pub fn load(&self, name: &str) -> Result<Mod> {
        let folder = self.mod_folder(name);
        if !folder.join("mod.json").exists() {
            bail!(
                "no mod {name:?}: expected {} (alchemists-tools mod new {name})",
                display_path(&folder.join("mod.json"))
            );
        }
        Mod::load(&folder)
    }

    fn mods_paks_folder(&self) -> PathBuf {
        join(&self.game.root, PAK_FOLDER)
    }

    fn record_path(&self) -> PathBuf {
        self.mods_paks_folder().join(RECORD)
    }

    pub fn record(&self) -> Result<Record> {
        let path = self.record_path();
        if !path.exists() {
            return Ok(Record::default());
        }
        Ok(serde_json::from_value(read_json(&path)?)?)
    }

    fn save_record(&self, record: &Record) -> Result<()> {
        write_json(&self.record_path(), record)
    }

    // --- craft types ---

    fn allocations_path(&self) -> PathBuf {
        self.mods.join("allocations.json")
    }

    fn allocations(&self) -> Result<IndexMap<String, u8>> {
        let path = self.allocations_path();
        if !path.exists() {
            return Ok(IndexMap::new());
        }
        let value = read_json(&path)?;
        Ok(serde_json::from_value(
            value.get("craft-types").cloned().unwrap_or(json!({})),
        )?)
    }

    /// Numbers for the mod's new craft types, kept in `<mods>/allocations.json` so they never
    /// change once given.
    fn allocate_craft_types(
        &self,
        name: &str,
        new_craft_types: &[String],
    ) -> Result<IndexMap<String, u8>> {
        let mut numbers = self.allocations()?;
        let mut changed = false;
        for craft_type in new_craft_types {
            let key = format!("{name}:{craft_type}");
            if !numbers.contains_key(&key) {
                let used: HashSet<u8> = numbers.values().copied().collect();
                let free = (FIRST_CRAFT_TYPE..=255)
                    .find(|number| !used.contains(number))
                    .context("no craft type numbers left (26-255)")?;
                numbers.insert(key.clone(), free);
                changed = true;
                println!("   new craft type {key} = {free}");
            }
        }
        if changed {
            write_json(&self.allocations_path(), &json!({"craft-types": numbers}))?;
        }
        Ok(new_craft_types
            .iter()
            .map(|craft_type| format!("{name}:{craft_type}"))
            .map(|key| (key.clone(), numbers[&key]))
            .collect())
    }

    fn known_craft_types(&self) -> Result<IndexMap<String, u8>> {
        let mut known = self.allocations()?;
        for entry in self.record()?.mods.values() {
            known.extend(entry.craft_type_numbers.clone());
        }
        Ok(known)
    }

    // --- dependencies ---

    /// A dependency, from the mods folder (built into an entry) or else from the installed mods,
    /// checked against its version requirement.
    fn find_dependency(&self, dependency: &Dependency, at: &str) -> Result<Entry> {
        // the tool carries its own Lua library: written when missing, updated when older
        if dependency.name == "AlchemistsLibrary" {
            crate::probe::write_library(&self.mods)?;
        }
        let local = self.mods.join(&dependency.name);
        let entry = if local.join("mod.json").exists() {
            let local_mod = Mod::load(&local)?;
            let numbers = self.allocate_craft_types(&local_mod.name, &local_mod.new_craft_types)?;
            let known = self.known_craft_types()?;
            let resolve = |files: &[(String, Value)]| -> Result<Vec<(String, Value)>> {
                files
                    .iter()
                    .map(|(label, data)| {
                        Ok((
                            label.clone(),
                            resolve_craft_types(data, &local_mod, &known, label)?,
                        ))
                    })
                    .collect()
            };
            // a dependency's assets, if it has been built
            let built = self.build.join(&local_mod.name).join("build.json");
            let assets = if built.exists() {
                serde_json::from_value::<Entry>(read_json(&built)?)
                    .map(|entry| entry.assets)
                    .unwrap_or_default()
            } else {
                IndexMap::new()
            };
            Entry {
                name: local_mod.name.clone(),
                title: local_mod.title.clone(),
                version: local_mod.version.clone(),
                dependencies: local_mod.dependencies.clone(),
                craft_type_numbers: numbers,
                edits: resolve(&local_mod.edits)?,
                optional_edits: local_mod
                    .optional_edits
                    .iter()
                    .map(|(key, files)| Ok((key.clone(), resolve(files)?)))
                    .collect::<Result<_>>()?,
                save_edits: local_mod.save_edits.clone(),
                assets,
                ..Entry::default()
            }
        } else {
            match self.record()?.mods.get(&dependency.name) {
                Some(entry) => entry.clone(),
                None => bail!(
                    "{at}: needs {}, which is neither in the mods folder nor installed",
                    requirement_text(dependency)
                ),
            }
        };
        if !satisfies(entry.version()?, dependency) {
            bail!(
                "{at}: needs {}, but {} is {}",
                requirement_text(dependency),
                dependency.name,
                entry.version
            );
        }
        Ok(entry)
    }

    /// The mods a mod needs, directly or not, in load order.
    fn required_mods(&self, name: &str, requires: &[Dependency]) -> Result<Vec<Entry>> {
        let mut required: IndexMap<String, Entry> = IndexMap::new();
        let mut pending: Vec<(String, Vec<Dependency>)> =
            vec![(name.to_string(), requires.to_vec())];
        while let Some((current, dependencies)) = pending.pop() {
            for dependency in dependencies {
                if dependency.kind.is_required() && !required.contains_key(&dependency.name) {
                    let entry = self.find_dependency(&dependency, &current)?;
                    pending.push((entry.name.clone(), entry.requires()?));
                    required.insert(dependency.name.clone(), entry);
                }
            }
        }
        if required.contains_key(name) {
            let names: Vec<&String> = required.keys().collect();
            bail!(
                "{name} requires itself through {}",
                names
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let order = load_order(&required)?;
        Ok(order
            .into_iter()
            .map(|name| required[&name].clone())
            .collect())
    }
}

/// Replace `"Mod:CraftType"` names with their numbers, in CraftType fields.
fn resolve_craft_types(
    value: &Value,
    mod_: &Mod,
    known: &IndexMap<String, u8>,
    at: &str,
) -> Result<Value> {
    Ok(match value {
        Value::Object(object) => {
            let mut resolved_fields = serde_json::Map::new();
            for (key, inner) in object {
                let resolved = if key == "CraftType" {
                    craft_type_number(inner, mod_, known, &format!("{at}.{key}"))?
                } else {
                    resolve_craft_types(inner, mod_, known, &format!("{at}.{key}"))?
                };
                resolved_fields.insert(key.clone(), resolved);
            }
            Value::Object(resolved_fields)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_craft_types(item, mod_, known, at))
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

fn craft_type_number(
    value: &Value,
    mod_: &Mod,
    known: &IndexMap<String, u8>,
    at: &str,
) -> Result<Value> {
    let Some(text) = value.as_str().filter(|text| text.contains(':')) else {
        return Ok(value.clone());
    };
    let owner = text.split(':').next().unwrap_or_default();
    let dependency = mod_
        .requires
        .iter()
        .any(|dependency| dependency.name == owner && dependency.kind != Kind::Incompatible);
    if owner != mod_.name && !dependency {
        bail!(
            "{at}: craft type {text} belongs to {owner}, which isn't among {}'s dependencies",
            mod_.name
        );
    }
    match known.get(text) {
        Some(number) => Ok(json!(number)),
        None => bail!(
            "{at}: unknown craft type {text} (a mod declares its own in \"new-craft-types\"; \
             build {owner} first)"
        ),
    }
}

/// UE4SS's own mods and modules (what its release ships), which aren't game mods.
const UE4SS_OWN: &[&str] = &[
    "BPML_GenericFunctions",
    "BPModLoaderMod",
    "CheatManagerEnablerMod",
    "ConsoleCommandsMod",
    "ConsoleEnablerMod",
    "Keybinds",
    "LineTraceMod",
    "SplitScreenMod",
    "jsbProfiler",
    "mods.txt",
    "mods.json",
    "shared/UEHelpers",
    "shared/Types.lua",
];

/// What a container holds: package paths (`/Game/...`) for a `.utoc`, file paths for a `.pak`.
fn container_contents(path: &Path) -> Vec<String> {
    if path
        .extension()
        .is_some_and(|extension| extension == "utoc")
    {
        return crate::unpack::package_names(path)
            .map(|names| names.into_iter().collect())
            .unwrap_or_default();
    }
    if path.extension().is_some_and(|extension| extension == "pak") {
        let Ok(file) = std::fs::File::open(path) else {
            return vec![];
        };
        let mut file = std::io::BufReader::new(file);
        if let Ok(reader) = repak::PakBuilder::new().reader(&mut file) {
            return reader.files();
        }
    }
    vec![]
}

/// What a template-save edit changes: (property, occurrence, after, part).
type SaveEditKey = (String, usize, Option<String>, Option<String>);

/// The install record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Record {
    pub mods: IndexMap<String, Entry>,
    pub load_order: Vec<String>,
    /// The files the tool generates (the merged containers), as it last wrote them: no mod owns
    /// them, and they're only replaced or removed while they're still exactly these.
    pub generated: IndexMap<String, FileRecord>,
}

/// Mod names with each mod after the mods it depends on (not `~`); otherwise alphabetical.
fn load_order(mods: &IndexMap<String, Entry>) -> Result<Vec<String>> {
    let mut must_load_after: HashMap<&String, BTreeSet<String>> = HashMap::new();
    for (name, entry) in mods {
        let before = entry
            .requires()?
            .into_iter()
            .filter(|dependency| {
                dependency.kind.affects_load_order() && mods.contains_key(&dependency.name)
            })
            .map(|dependency| dependency.name)
            .collect();
        must_load_after.insert(name, before);
    }
    let mut order: Vec<String> = vec![];
    while order.len() < mods.len() {
        let placed: BTreeSet<String> = order.iter().cloned().collect();
        let mut ready: Vec<&String> = mods
            .keys()
            .filter(|name| !placed.contains(*name) && must_load_after[name].is_subset(&placed))
            .collect();
        ready.sort();
        let Some(next) = ready.first() else {
            let mut stuck: Vec<&String> =
                mods.keys().filter(|name| !placed.contains(*name)).collect();
            stuck.sort();
            let stuck: Vec<&str> = stuck.iter().map(|name| name.as_str()).collect();
            bail!(
                "dependency loop between {}: make one of the dependencies \"~\" (doesn't affect \
                load order)",
                stuck.join(", ")
            );
        };
        order.push((*next).clone());
    }
    Ok(order)
}

/// The installed mods that load before `name` because of its dependencies, directly or not.
fn ancestors(name: &str, mods: &IndexMap<String, Entry>) -> Result<HashSet<String>> {
    let mut ancestors = HashSet::new();
    let mut pending = vec![name.to_string()];
    while let Some(current) = pending.pop() {
        for dependency in mods[&current].requires()? {
            if dependency.kind.affects_load_order()
                && mods.contains_key(&dependency.name)
                && ancestors.insert(dependency.name.clone())
            {
                pending.push(dependency.name);
            }
        }
    }
    Ok(ancestors)
}

/// A mod's edit files, plus those for the optional dependencies that are present.
fn table_edits_of(entry: &Entry, present: &HashSet<String>) -> Vec<edits::Source> {
    let mut sources: Vec<edits::Source> = entry
        .edits
        .iter()
        .map(|(label, contents)| edits::Source {
            label: label.clone(),
            contents: contents.clone(),
        })
        .collect();
    let mut others: Vec<&String> = entry.optional_edits.keys().collect();
    others.sort();
    for other in others {
        if present.contains(other) {
            sources.extend(entry.optional_edits[other].iter().map(|(label, contents)| {
                edits::Source {
                    label: label.clone(),
                    contents: contents.clone(),
                }
            }));
        }
    }
    sources
}

fn leaf_values(value: &Value, path: Vec<String>, collected: &mut Vec<(Vec<String>, Value)>) {
    match value {
        Value::Object(object) => {
            for (key, inner) in object {
                if !key.starts_with('_') {
                    let mut next = path.clone();
                    next.push(key.clone());
                    leaf_values(inner, next, collected);
                }
            }
        }
        other => collected.push((path, other.clone())),
    }
}

fn copy_folder(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Every file below `folder`, as paths relative to it with `/`.
fn files_below(folder: &Path) -> Vec<String> {
    let mut files = vec![];
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(folder) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    files
}

/// The modules a Lua script loads with `require("Name")` (or `require "Name"`).
fn required_modules(script: &str) -> Vec<String> {
    let mut modules = vec![];
    let mut remaining = script;
    while let Some(at) = remaining.find("require") {
        remaining = &remaining[at + "require".len()..];
        let after = remaining.trim_start().trim_start_matches('(').trim_start();
        if let Some(quote) = after
            .chars()
            .next()
            .filter(|character| *character == '"' || *character == '\'')
        {
            if let Some(end) = after[1..].find(quote) {
                modules.push(after[1..1 + end].to_string());
            }
        }
    }
    modules
}

impl Workspace {
    /// Build a mod into `<build>/<name>/`: its tables (on top of the mods it requires), its cooked
    /// content, its template save, and its UE4SS files; then check them against each other.
    pub fn build_mod(&self, mod_: &Mod) -> Result<(PathBuf, Entry)> {
        let name = &mod_.name;
        let build_folder = self.build.join(name);
        let _ = std::fs::remove_dir_all(&build_folder);
        let paks = join(&build_folder, PAK_FOLDER);
        std::fs::create_dir_all(&paks)?;
        // first, so their craft types are known
        let needed = self.required_mods(name, &mod_.requires)?;
        let own_numbers = self.allocate_craft_types(name, &mod_.new_craft_types)?;
        let known = self.known_craft_types()?;
        let resolve = |files: &[(String, Value)]| -> Result<Vec<(String, Value)>> {
            files
                .iter()
                .map(|(label, data)| {
                    Ok((
                        label.clone(),
                        resolve_craft_types(data, mod_, &known, label)?,
                    ))
                })
                .collect()
        };
        let own_edits = resolve(&mod_.edits)?;
        let optional: IndexMap<String, Vec<(String, Value)>> = mod_
            .optional_edits
            .iter()
            .map(|(key, files)| Ok((key.clone(), resolve(files)?)))
            .collect::<Result<_>>()?;
        let schemas = self.schemas()?;
        let mut machine_types: IndexMap<String, u8> = IndexMap::new();
        for (asset, value) in &mod_.craft_types {
            let value =
                craft_type_number(value, mod_, &known, &format!("{name}/mod.json craft-types"))?;
            let text = match &value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            machine_types.insert(asset.clone(), content::craft_type_number(&text, schemas)?);
        }
        let mut info = Entry {
            name: name.clone(),
            title: mod_.title.clone(),
            version: mod_.version.clone(),
            game_build: game_build(&self.game),
            dependencies: mod_.dependencies.clone(),
            craft_type_numbers: own_numbers,
            edits: own_edits.clone(),
            optional_edits: optional.clone(),
            save_edits: mod_.save_edits.clone(),
            ..Entry::default()
        };

        let mut built = None;
        if !own_edits.is_empty() {
            let labels: Vec<&str> = own_edits.iter().map(|(label, _)| label.as_str()).collect();
            let on_top: Vec<&str> = needed.iter().map(|entry| entry.name.as_str()).collect();
            println!(
                "== table edits ({}){}",
                labels.join(", "),
                if on_top.is_empty() {
                    String::new()
                } else {
                    format!(", on top of {}", on_top.join(", "))
                }
            );
            let mut sources: Vec<edits::Source> = needed
                .iter()
                .flat_map(|entry| table_edits_of(entry, &HashSet::new()))
                .collect();
            sources.extend(own_edits.iter().map(|(label, contents)| edits::Source {
                label: label.clone(),
                contents: contents.clone(),
            }));
            let result = edits::build(&self.game_for_edits()?, &sources)?;
            let utoc = paks.join(format!("{}.utoc", data_container(name)));
            write_container(
                &utoc,
                &result.tables,
                retoc::version::EngineVersion::UE5_6,
                script_objects(&self.game)?,
            )?;
            check_container(&utoc, &result.tables, &self.game)?;
            let rows: usize = result.changed.values().map(|rows| rows.len()).sum();
            println!(
                ".pak mod: {rows} row(s) in {} table(s), checked",
                result.changed.len()
            );
            for line in &result.report {
                println!("   {line}");
            }
            for warning in &result.warnings {
                println!("   warning: {warning}");
            }
            info.tables = result.changed.keys().cloned().collect();
            info.tables.sort();
            if !optional.is_empty() {
                let others: Vec<&str> = optional.keys().map(|name| name.as_str()).collect();
                println!(
                    "   edits for when {} are installed are checked when they're merged",
                    others.join(", ")
                );
            }
            built = Some(result);
        }

        if !mod_.content.is_empty() {
            println!("== cooked content ({})", mod_.content.join(", "));
            content::check_paths(&mod_.content, false)?;
            let stale: Vec<String> = mod_
                .content
                .iter()
                .flat_map(|path| content::stale_cooks(&self.project, path))
                .collect();
            if !stale.is_empty() {
                bail!(
                    "the cook is out of date for:\n   {}\n\
                     cook again (Platforms -> Windows -> Cook Content)",
                    stale.join("\n   ")
                );
            }
            let mut staged = content::stage(&self.project, &mod_.content)?;
            for (asset, &value) in &machine_types {
                let staged_asset = staged
                    .iter_mut()
                    .find(|staged| staged.path.rsplit('/').next() == Some(asset.as_str()))
                    .with_context(|| {
                        format!("craft type for {asset}: no {asset} among the cooked assets")
                    })?;
                let old = content::patch_craft_type(staged_asset, value)?;
                println!("   craft type {old} -> {value} in {asset}");
            }
            let utoc = paks.join(format!("{}.utoc", content_container(name)));
            let packaged = content::package(&utoc, &staged, &self.game, schemas, &machine_types)?;
            for (package, components) in &packaged.components {
                println!("   {package}:");
                for component in components {
                    println!("      {}", content::describe(component));
                }
            }
            info.assets = packaged.assets;
        }

        if !mod_.save_edits.is_empty() {
            println!("== template save (save/DefaultGame.json)");
            let edits: Vec<SaveEdit> = needed
                .iter()
                .flat_map(|entry| entry.save_edits.clone())
                .chain(mod_.save_edits.clone())
                .collect();
            self.write_save(&edits, &paks, &save_container(name))?;
        }

        let scripts = mod_.folder.join("ue4ss");
        if scripts.is_dir() {
            println!("== UE4SS mod ({})", display_path(&scripts));
            // A Lua mod has Scripts/main.lua, a C++ mod dlls/main.dll (UE4SS loads either from the folder)
            if !scripts.join("Scripts").join("main.lua").exists()
                && !scripts.join("dlls").join("main.dll").exists()
            {
                bail!(
                    "{}: a UE4SS mod needs Scripts/main.lua (Lua) or dlls/main.dll (C++)",
                    display_path(&scripts)
                );
            }
            let target = join(&build_folder, UE4SS_FOLDER).join(name);
            copy_folder(&scripts, &target)?;
            std::fs::write(target.join("enabled.txt"), "")?;
            info.needs_ue4ss = true;
        }
        let shared = mod_.folder.join("ue4ss-shared");
        if shared.is_dir() {
            println!("== UE4SS shared Lua modules ({})", display_path(&shared));
            for entry in std::fs::read_dir(&shared)?.flatten() {
                let module = entry.file_name().to_string_lossy().into_owned();
                if !module.starts_with(name.as_str()) {
                    bail!(
                        "{}: shared modules are named after their mod ({name}...), so no two mods \
                         install the same one",
                        display_path(&entry.path())
                    );
                }
                if !entry.path().join(format!("{module}.lua")).exists() {
                    bail!(
                        "{}: needs {module}.lua, which require(\"{module}\") loads",
                        display_path(&entry.path())
                    );
                }
                copy_folder(
                    &entry.path(),
                    &join(&build_folder, UE4SS_FOLDER)
                        .join("shared")
                        .join(&module),
                )?;
            }
            info.needs_ue4ss = true;
        }
        // A script that requires another mod's shared module needs that mod as a dependency
        let mut libraries: HashMap<String, String> = HashMap::new();
        for entry in std::fs::read_dir(&self.mods)
            .into_iter()
            .flatten()
            .flatten()
        {
            for module in std::fs::read_dir(entry.path().join("ue4ss-shared"))
                .into_iter()
                .flatten()
                .flatten()
            {
                libraries.insert(
                    module.file_name().to_string_lossy().into_owned(),
                    entry.file_name().to_string_lossy().into_owned(),
                );
            }
        }
        let depends: HashSet<&str> = mod_
            .requires
            .iter()
            .filter(|dependency| dependency.kind != Kind::Incompatible)
            .map(|dependency| dependency.name.as_str())
            .chain([name.as_str()])
            .collect();
        for folder in [&scripts, &shared] {
            for file in files_below(folder)
                .into_iter()
                .filter(|file| file.ends_with(".lua"))
            {
                let text = std::fs::read_to_string(folder.join(&file)).unwrap_or_default();
                for module in required_modules(&text) {
                    if let Some(owner) = libraries
                        .get(&module)
                        .filter(|owner| !depends.contains(owner.as_str()))
                    {
                        bail!(
                            "{file} requires {module}, from the mod {owner}: add \"{owner}\" to \
                            its dependencies in mod.json"
                        );
                    }
                }
            }
        }

        // New references into /Game/Mods must be cooked assets of this mod, or of a mod it
        // requires,
        // of the class the table field holds
        let mut problems = vec![];
        let mut available = info.assets.clone();
        for entry in &needed {
            available.extend(entry.assets.clone());
        }
        if let Some(result) = &built {
            for (path, class) in &result.references {
                if !path.starts_with("/Game/Mods/") {
                    continue;
                }
                let short = |class: &str| class.rsplit('.').next().unwrap_or(class).to_string();
                match available.get(path) {
                    None => {
                        let package = path.split('.').next().unwrap_or(path);
                        let near: Vec<&str> = available
                            .keys()
                            .filter(|asset| asset.split('.').next() == Some(package))
                            .map(|asset| asset.as_str())
                            .collect();
                        problems.push(format!(
                            "the edits refer to {path}, which neither this mod nor a mod it \
                            requires packages{}",
                            if near.is_empty() {
                                " (add its folder to \"content\" in mod.json, and cook)".to_string()
                            } else {
                                format!(" (that package has {})", near.join(", "))
                            }
                        ));
                    }
                    Some(found) if found != class => problems.push(format!(
                        "the edits use {path} as a {}, but it's a {}",
                        short(class),
                        short(found)
                    )),
                    _ => {}
                }
            }
        }
        // Craft types past Max: the recipes and the machines must agree
        let named: HashSet<u8> = schemas
            .enums
            .get("EBeltTDCraftType")
            .map(|values| values.keys().filter_map(|key| key.parse().ok()).collect())
            .unwrap_or_default();
        let mut machines: HashSet<u8> = machine_types.values().copied().collect();
        machines.extend(
            needed
                .iter()
                .flat_map(|entry| entry.craft_type_numbers.values().copied()),
        );
        let mut recipe_types: HashSet<u8> = HashSet::new();
        if let Some(result) = &built {
            for (row, recipe) in result.changed.get("DT_EnemyCrafting").into_iter().flatten() {
                if let Some(number) = recipe.get("CraftType").and_then(Value::as_u64) {
                    let number = number as u8;
                    recipe_types.insert(number);
                    if !named.contains(&number) && !machines.contains(&number) {
                        println!(
                            "warning: recipe {row} has craft type {number}, which no machine \
                            in this mod or the mods it requires runs"
                        );
                    }
                }
            }
        }
        for number in machine_types.values() {
            if !named.contains(number) && !recipe_types.contains(number) {
                println!(
                    "warning: craft type {number} has no recipes in this mod, so its \
                    machine's list is empty"
                );
            }
        }
        // The rows this mod adds (not those of the mods it requires), and which are buildings
        if let Some(result) = &built {
            let own: HashSet<(String, String)> = own_edits
                .iter()
                .flat_map(|(_, data)| {
                    data.as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(table, _)| !table.starts_with('_'))
                        .flat_map(|(table, rows)| {
                            rows.as_object()
                                .into_iter()
                                .flatten()
                                .map(move |(row, _)| (table.clone(), row.clone()))
                        })
                })
                .collect();
            let mut new_rows: Vec<(String, String)> = result
                .new_rows
                .iter()
                .map(|(t, r, _)| (t.clone(), r.clone()))
                .filter(|key| own.contains(key))
                .collect();
            new_rows.sort();
            for (table, row) in &new_rows {
                if table == "DT_Buildings" {
                    let display = result.changed[table]
                        .get(row)
                        .and_then(|row| row.get("DisplayName"))
                        .and_then(Value::as_str);
                    info.new_buildings.push(
                        display
                            .filter(|text| !text.is_empty())
                            .unwrap_or(row)
                            .to_string(),
                    );
                }
            }
            info.new_rows = new_rows;
        }
        if !problems.is_empty() {
            bail!("build failed:\n   {}", problems.join("\n   "));
        }

        let built_files = files_below(&build_folder);
        if built_files.is_empty() {
            bail!(
                "{}: nothing to build (no edits/*.json, save/, content in mod.json, ue4ss/ or \
                ue4ss-shared/)",
                display_path(&mod_.folder)
            );
        }
        // The mod's own data container and template save are for the zip; installing merges them
        let prefix = PAK_FOLDER.join("/");
        let own_data = |file: &String| {
            [data_container(name), save_container(name)]
                .iter()
                .any(|base| file.starts_with(&format!("{prefix}/{base}.")))
        };
        info.data_files = built_files
            .iter()
            .filter(|file| own_data(file))
            .cloned()
            .collect();
        info.files = built_files
            .iter()
            .filter(|file| !own_data(file) && *file != "build.json")
            .cloned()
            .collect();
        write_json(&build_folder.join("build.json"), &info)?;
        println!(
            "built {name} {} -> {} ({} file(s))",
            mod_.version,
            display_path(&build_folder),
            built_files.len()
        );
        Ok((build_folder, info))
    }

    /// The game's template save with `edits`, as `<folder>/<base>.pak` with its empty container.
    fn write_save(&self, edits: &[SaveEdit], folder: &Path, base: &str) -> Result<()> {
        let template = crate::unpack::pak_file(&self.game.pak(), save::TEMPLATE)?;
        let (edited, old) = save::edit_save(&template, edits).context("template save")?;
        for (edit, old) in edits.iter().zip(old) {
            let place = format!(
                "{}{}",
                if edit.occurrence > 0 {
                    format!(" number {}", edit.occurrence + 1)
                } else {
                    String::new()
                },
                edit.after
                    .as_ref()
                    .map(|after| format!(" after {after}"))
                    .unwrap_or_default()
            );
            println!(
                "   template save: {}{place} {old} -> {}",
                edit.property, edit.value
            );
        }
        std::fs::create_dir_all(folder)?;
        save::write_template_pak(folder, base, &edited, &self.game)
    }

    // --- merging installed mods ---

    fn report_clashes(&self, order: &[String], mods: &IndexMap<String, Entry>) -> Result<()> {
        let assets = self.assets()?;
        let schemas = self.schemas()?;
        let mut game_rows: HashMap<String, HashSet<String>> = HashMap::new();
        let mut is_game_row = |table: &str, row: &str| -> bool {
            game_rows
                .entry(table.to_string())
                .or_insert_with(|| {
                    let matches = assets.find(table);
                    let rows = matches
                        .first()
                        .and_then(|path| assets.asset(path).ok())
                        .and_then(|asset| read_datatable(asset, schemas, None).ok());
                    rows.and_then(|decoded| {
                        decoded["rows"]
                            .as_object()
                            .map(|rows| rows.keys().cloned().collect())
                    })
                    .unwrap_or_default()
                })
                .contains(row)
        };
        let present: HashSet<String> = order.iter().cloned().collect();
        let related: HashMap<&String, HashSet<String>> = order
            .iter()
            .map(|name| Ok((name, ancestors(name, mods)?)))
            .collect::<Result<_>>()?;
        let mut set_by: HashMap<Vec<String>, (String, Value)> = HashMap::new();
        let mut made_by: HashMap<(String, String, String), String> = HashMap::new();
        let mut problems = vec![];
        for name in order {
            for source in table_edits_of(&mods[name], &present) {
                for (table, rows) in source.contents.as_object().into_iter().flatten() {
                    if table.starts_with('_') {
                        continue;
                    }
                    for (row, fields) in rows.as_object().into_iter().flatten() {
                        if row.starts_with('_') || !fields.is_object() {
                            continue;
                        }
                        let mut claims = vec![];
                        if !is_game_row(table, row) {
                            claims.push((
                                ("new row".to_string(), table.clone(), row.clone()),
                                "add the row",
                            ));
                        }
                        if let Some(slot) = fields.get("_slot").and_then(Value::as_str) {
                            claims.push((
                                ("slot".to_string(), table.clone(), slot.to_string()),
                                "put a recipe in the unused row",
                            ));
                        }
                        for (key, what) in claims {
                            let first = made_by
                                .entry(key.clone())
                                .or_insert_with(|| name.clone())
                                .clone();
                            if first != *name && !related[name].contains(&first) {
                                problems.push(format!(
                                    "{first} and {name} both {what} {table}.{}",
                                    key.2
                                ));
                            }
                        }
                        let mut values_set = vec![];
                        leaf_values(fields, vec![table.clone(), row.clone()], &mut values_set);
                        for (path, value) in values_set {
                            if let Some((first, old)) = set_by.get(&path) {
                                if first != name && *old != value && !related[name].contains(first)
                                {
                                    println!(
                                        "warning: {first} and {name} both set {}; {name}'s value \
                                        is used (it loads later). \
                                         Make one depend on the other to decide.",
                                        path.join(".")
                                    );
                                }
                            }
                            set_by.insert(path, (name.clone(), value));
                        }
                    }
                }
            }
        }
        if !problems.is_empty() {
            bail!("mods clash:\n   {}", problems.join("\n   "));
        }
        Ok(())
    }

    /// Build the merged template save into a work folder; None when no mod edits it.
    fn merge_save(
        &self,
        order: &[String],
        mods: &IndexMap<String, Entry>,
    ) -> Result<Option<PathBuf>> {
        let mut edits = vec![];
        let mut set_by: HashMap<SaveEditKey, (String, Value)> = HashMap::new();
        for name in order {
            let related = ancestors(name, mods)?;
            for edit in &mods[name].save_edits {
                let key = (
                    edit.property.clone(),
                    edit.occurrence,
                    edit.after.clone(),
                    edit.part.clone(),
                );
                if let Some((first, value)) = set_by.get(&key) {
                    if first != name && *value != edit.value && !related.contains(first) {
                        println!(
                            "warning: {first} and {name} both change {} in the template save; \
                            {name}'s value is used (it loads later)",
                            edit.property
                        );
                    }
                }
                set_by.insert(key, (name.clone(), edit.value.clone()));
                edits.push(edit.clone());
            }
        }
        if edits.is_empty() {
            return Ok(None);
        }
        let with_edits: Vec<&str> = order
            .iter()
            .filter(|name| !mods[*name].save_edits.is_empty())
            .map(|name| name.as_str())
            .collect();
        println!(
            "== merging the template-save edits of {}",
            with_edits.join(", ")
        );
        let work = self.build.join(".mergesave");
        let _ = std::fs::remove_dir_all(&work);
        self.write_save(&edits, &work, &save_container(MERGED_SAVE))?;
        Ok(Some(work))
    }

    /// Check that the tool's merged files `~mods/<base>.*` are exactly what it last wrote (as
    /// recorded), before any is replaced; a record from before the tool kept hashes accepts the
    /// merged files it finds once. With `force`, a changed or unrecorded one is replaced anyway.
    fn check_generated(
        &self,
        files: &GameFiles,
        record: &Record,
        base: &str,
        force: bool,
    ) -> Result<()> {
        let legacy = record.generated.is_empty();
        let prefix = PAK_FOLDER.join("/");
        for extension in ["pak", "utoc", "ucas"] {
            let relative = format!("{prefix}/{base}.{extension}");
            if !files.resolve(&relative)?.exists() {
                continue;
            }
            let problem = match record.generated.get(&relative) {
                Some(recorded) if files.state(recorded)? == State::Unchanged => continue,
                Some(_) => "has changed since the tool wrote it",
                None if legacy => continue,
                None => "wasn't written by the tool (it has no record of it)",
            };
            if force {
                println!("warning: {relative} {problem}; replacing it anyway (--force)");
                continue;
            }
            bail!(
                "{relative} {problem}, so it isn't replaced; move it away (it may be another \
                 mod's) and run this again, or pass --force to replace it"
            );
        }
        Ok(())
    }

    /// Replace the tool's merged files `~mods/<base>.*` with those in `work` (or remove them if
    /// None), once [`Self::check_generated`] has passed. Returns whether any were removed.
    fn replace_generated(
        &self,
        files: &GameFiles,
        record: &mut Record,
        base: &str,
        work: Option<&Path>,
    ) -> Result<bool> {
        let prefix = PAK_FOLDER.join("/");
        let mut removed = false;
        for extension in ["pak", "utoc", "ucas"] {
            let relative = format!("{prefix}/{base}.{extension}");
            let full = files.resolve(&relative)?;
            if !full.exists() {
                record.generated.shift_remove(&relative);
                continue;
            }
            std::fs::remove_file(&full).with_context(|| format!("removing {relative}"))?;
            record.generated.shift_remove(&relative);
            removed = true;
        }
        if let Some(work) = work {
            std::fs::create_dir_all(self.mods_paks_folder())?;
            for file in files_below(work) {
                let relative = format!("{prefix}/{file}");
                std::fs::copy(work.join(&file), files.resolve(&relative)?)?;
                record
                    .generated
                    .insert(relative.clone(), files.record(&relative)?);
            }
        }
        Ok(removed)
    }

    /// Rebuild `zzz_ModTables_P` (and `zzz_ModSave_P`) from every installed mod's edits.
    fn merge(&self, record: &mut Record, force: bool) -> Result<()> {
        let order = load_order(&record.mods)?;
        self.report_clashes(&order, &record.mods)?;
        let present: HashSet<String> = order.iter().cloned().collect();
        let sources: Vec<edits::Source> = order
            .iter()
            .flat_map(|name| table_edits_of(&record.mods[name], &present))
            .collect();
        record.load_order = order.clone();
        let files = GameFiles::new(&self.game.root)?;
        let base = data_container(MERGED);
        let save_base = save_container(MERGED_SAVE);
        // every merged file is checked before any is touched, and both are built before either
        // is replaced, so a refusal or a failed build leaves the game folder as it was
        self.check_generated(&files, record, &base, force)?;
        self.check_generated(&files, record, &save_base, force)?;
        let save_work = self.merge_save(&order, &record.mods)?;
        let mut written = None;
        if !sources.is_empty() {
            // build first: if it fails, the installed container stays as it was
            println!("== merging the table edits of {}", order.join(", "));
            let result = edits::build(&self.game_for_edits()?, &sources)?;
            for warning in &result.warnings {
                println!("   warning: {warning}");
            }
            let work = self.build.join(".mergework");
            let _ = std::fs::remove_dir_all(&work);
            let utoc = work.join(format!("{base}.utoc"));
            if !result.tables.is_empty() {
                write_container(
                    &utoc,
                    &result.tables,
                    retoc::version::EngineVersion::UE5_6,
                    script_objects(&self.game)?,
                )?;
                check_container(&utoc, &result.tables, &self.game)?;
                written = Some(work);
            }
        }
        if self.replace_generated(&files, record, &save_base, save_work.as_deref())?
            && save_work.is_none()
        {
            println!("no installed mod edits the template save any more: removed the merged one");
        }
        if let Some(work) = save_work {
            let _ = std::fs::remove_dir_all(&work);
        }
        let removed = self.replace_generated(&files, record, &base, written.as_deref())?;
        match written {
            Some(work) => drop(std::fs::remove_dir_all(&work)),
            None if removed => println!(
                "no installed mod edits tables any more: removed the merged data container"
            ),
            None => {}
        }
        Ok(())
    }

    /// Re-merge the installed mods' edits against the game's current tables (after a game update).
    pub fn remerge(&self) -> Result<()> {
        let mut record = self.record()?;
        if record.mods.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = record.mods.keys().map(|name| name.as_str()).collect();
        println!(
            "   re-merging the installed mods ({}) against the new tables",
            names.join(", ")
        );
        self.merge(&mut record, false)?;
        self.save_record(&record)
    }

    // --- installing ---

    fn check_installable(&self, info: &Entry, installed: &IndexMap<String, Entry>) -> Result<()> {
        let mut problems = vec![];
        let own_version = info.version()?;
        for dependency in info.requires()? {
            let other = installed.get(&dependency.name);
            match (dependency.kind, other) {
                (Kind::Incompatible, Some(_)) => problems.push(format!(
                    "{} doesn't work with {}, which is installed",
                    info.name, dependency.name
                )),
                (Kind::Incompatible, None) => {}
                (kind, None) if kind.is_required() => problems.push(format!(
                    "{} needs {}: install it first, or use --with-dependencies",
                    info.name,
                    requirement_text(&dependency)
                )),
                (Kind::Recommended, None) => {
                    println!(
                        "note: {} recommends {}",
                        info.name,
                        requirement_text(&dependency)
                    )
                }
                (_, None) => {}
                (_, Some(other)) => {
                    if !satisfies(other.version()?, &dependency) {
                        problems.push(format!(
                            "{} needs {}, but {} is installed",
                            info.name,
                            requirement_text(&dependency),
                            other.version
                        ));
                    }
                }
            }
        }
        for (name, other) in installed {
            if *name == info.name {
                continue;
            }
            for dependency in other.requires()? {
                if dependency.name != info.name {
                    continue;
                }
                if dependency.kind == Kind::Incompatible {
                    problems.push(format!(
                        "{name} (installed) doesn't work with {}",
                        info.name
                    ));
                } else if !satisfies(own_version, &dependency) {
                    problems.push(format!(
                        "{name} (installed) needs {}, not {}",
                        requirement_text(&dependency),
                        info.version
                    ));
                }
            }
        }
        let mut numbers: HashMap<u8, String> = HashMap::new();
        for entry in installed
            .values()
            .filter(|entry| entry.name != info.name)
            .chain([info])
        {
            for (key, number) in &entry.craft_type_numbers {
                let first = numbers
                    .entry(*number)
                    .or_insert_with(|| key.clone())
                    .clone();
                if first != *key {
                    problems.push(format!(
                        "craft type {number} is both {first} and {key} (mods made with different \
                        allocations.json \
                         files; one of them needs a new number)"
                    ));
                }
            }
        }
        if !problems.is_empty() {
            bail!("can't install:\n   {}", problems.join("\n   "));
        }
        Ok(())
    }

    /// The files to install for a mod, as records (path, size, hash) of its build.
    fn file_records(build_folder: &Path, relative: &[String]) -> Result<Vec<FileRecord>> {
        relative
            .iter()
            .map(|path| {
                let (size, sha256) = hash(&build_folder.join(path))?;
                Ok(FileRecord {
                    path: path.clone(),
                    size,
                    sha256,
                })
            })
            .collect()
    }

    /// Stop unless every file the mod would write is free for it: no other mod's manifest lists
    /// it, the tool didn't generate it, and if it exists it's this mod's own (as its manifest, or
    /// for a mod installed before manifests, the install record, says) and unchanged.
    fn check_files_free(
        &self,
        files: &GameFiles,
        name: &str,
        new_files: &[FileRecord],
        record: &Record,
        manifests: &[(String, Manifest)],
    ) -> Result<()> {
        let previous = manifests
            .iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, manifest)| manifest);
        let mut problems = vec![];
        for file in new_files {
            let full = files.resolve(&file.path)?;
            if let Some((owner, _)) = manifests.iter().find(|(owner, manifest)| {
                owner != name && manifest.files.iter().any(|listed| listed.path == file.path)
            }) {
                problems.push(format!("{} belongs to the mod {owner}", file.path));
                continue;
            }
            if record.generated.contains_key(&file.path) {
                problems.push(format!("{} is one of the tool's merged files", file.path));
                continue;
            }
            if !full.exists() {
                continue;
            }
            match previous.and_then(|manifest| {
                manifest
                    .files
                    .iter()
                    .find(|listed| listed.path == file.path)
            }) {
                Some(old) if files.state(old)? == State::Unchanged => {}
                Some(_) => problems.push(format!(
                    "{} has changed since {name} installed it (move it away, or uninstall {name} \
                     with --force, first)",
                    file.path
                )),
                None => {
                    let recorded = record
                        .mods
                        .get(name)
                        .is_some_and(|entry| entry.files.contains(&file.path));
                    if !recorded {
                        problems.push(format!(
                            "{} already exists, and no manifest says it's {name}'s \
                            (alchemists-tools \
                             mod scan shows what it is; move it away first)",
                            file.path
                        ));
                    }
                }
            }
        }
        if !problems.is_empty() {
            bail!(
                "can't install {name}: its files would replace files that aren't its own:\n   {}",
                problems.join("\n   ")
            );
        }
        Ok(())
    }

    fn write_manifest(&self, files: &GameFiles, manifest: &Manifest) -> Result<()> {
        let path = files.resolve(&GameFiles::manifest_path(&manifest.name))?;
        write_json(&path, manifest)
    }

    fn manifest_for(info: &Entry, installed_by: &str, files: Vec<FileRecord>) -> Manifest {
        Manifest {
            name: info.name.clone(),
            title: info.title.clone(),
            version: info.version.clone(),
            game_build: info.game_build.clone(),
            dependencies: info.dependencies.clone(),
            installed_by: installed_by.to_string(),
            tables: info.tables.clone(),
            new_buildings: info.new_buildings.clone(),
            files,
        }
    }

    pub fn install(
        &self,
        mod_: &Mod,
        with_dependencies: bool,
        force: bool,
        installing: &[String],
    ) -> Result<()> {
        if installing.contains(&mod_.name) {
            bail!(
                "dependency loop: {} -> {}",
                installing.join(" -> "),
                mod_.name
            );
        }
        if with_dependencies {
            for dependency in mod_
                .requires
                .iter()
                .filter(|dependency| dependency.kind.is_required())
            {
                let current = self.record()?.mods.get(&dependency.name).cloned();
                let local = self.mods.join(&dependency.name).join("mod.json");
                let newer = match (&current, local.exists()) {
                    (Some(current), true) => Mod::load(&self.mods.join(&dependency.name))
                        .ok()
                        .and_then(|local| parse_version(&local.version, &dependency.name).ok())
                        .is_some_and(|local| {
                            current.version().is_ok_and(|installed| local > installed)
                        }),
                    _ => false,
                };
                let unmet = current.as_ref().is_none_or(|current| {
                    current
                        .version()
                        .is_ok_and(|version| !satisfies(version, dependency))
                });
                if unmet || newer {
                    // the tool carries its own Lua library, for mods that depend on it: written
                    // when missing, and updated when the copy is older than the tool's
                    if dependency.name == "AlchemistsLibrary" {
                        crate::probe::write_library(&self.mods)?;
                    }
                    if !local.exists() {
                        bail!(
                            "{} needs {}, which isn't in the mods folder",
                            mod_.name,
                            requirement_text(dependency)
                        );
                    }
                    let mut chain = installing.to_vec();
                    chain.push(mod_.name.clone());
                    self.install(
                        &Mod::load(&self.mods.join(&dependency.name))?,
                        true,
                        force,
                        &chain,
                    )?;
                }
            }
        }
        let (build_folder, info) = self.build_mod(mod_)?;
        if info.needs_ue4ss && !join(&self.game.root, UE4SS_FOLDER).is_dir() {
            bail!(
                "{} needs UE4SS, and it isn't installed in {}",
                info.name,
                display_path(&self.game.root)
            );
        }
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        self.check_installable(&info, &record.mods)?;
        let new_files = Self::file_records(&build_folder, &info.files)?;
        let manifests = files.manifests()?;
        self.check_files_free(&files, &info.name, &new_files, &record, &manifests)?;
        // this mod's old files that the new version doesn't have, deleted once it's in
        let stale: Vec<FileRecord> = manifests
            .iter()
            .find(|(owner, _)| *owner == info.name)
            .map(|(_, manifest)| {
                manifest
                    .files
                    .iter()
                    .filter(|old| !new_files.iter().any(|new| new.path == old.path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        record.mods.insert(info.name.clone(), info.clone());
        self.merge(&mut record, force)?;
        for file in &new_files {
            let target = files.resolve(&file.path)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(build_folder.join(&file.path), &target)
                .with_context(|| format!("installing {}", file.path))?;
        }
        let gone = files.delete_listed(&stale, false)?;
        for kept in &gone.kept {
            println!(
                "   kept {kept}: no longer part of {}, but changed since it was installed",
                info.name
            );
        }
        self.write_manifest(
            &files,
            &Self::manifest_for(&info, "alchemists-tools", new_files),
        )?;
        self.save_record(&record)?;
        println!(
            "installed {} {} (load order: {}); restart the game to load it",
            info.name,
            info.version,
            record.load_order.join(", ")
        );
        if !info.new_buildings.is_empty() {
            println!(
                "   before uninstalling, deconstruct every placed {} in every save you keep: a \
                save with \
                 one placed crashes the game without the mod",
                info.new_buildings.join(", ")
            );
        }
        let unknown = self.unowned_containers(&files, &record)?;
        if !unknown.is_empty() {
            println!(
                "warning: ~mods has containers no manifest lists ({}); if one replaces a table \
                that installed \
                 mods edit, only one takes effect (alchemists-tools mod scan shows what they \
                     change)",
                unknown.join(", ")
            );
        }
        Ok(())
    }

    /// Containers in `~mods` that no manifest or record lists and the tool didn't generate.
    fn unowned_containers(&self, files: &GameFiles, record: &Record) -> Result<Vec<String>> {
        let owned: HashSet<String> = files
            .manifests()?
            .into_iter()
            .flat_map(|(_, manifest)| manifest.files.into_iter().map(|file| file.path))
            .chain(record.generated.keys().cloned())
            .chain(
                record
                    .mods
                    .values()
                    .flat_map(|entry| entry.files.iter().cloned()),
            )
            .collect();
        let prefix = PAK_FOLDER.join("/");
        let mut containers: Vec<String> = std::fs::read_dir(self.mods_paks_folder())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".utoc") || name.ends_with(".pak"))
            .filter(|name| !owned.contains(&format!("{prefix}/{name}")))
            .map(|name| {
                name.rsplit_once('.')
                    .map(|(stem, _)| stem.to_string())
                    .unwrap_or(name)
            })
            .collect();
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    /// Files that look like a mod's by their names, for a mod no manifest or record describes.
    fn files_named_for(&self, name: &str) -> Vec<String> {
        let prefix = PAK_FOLDER.join("/");
        let mut files: Vec<String> = [
            data_container(name),
            content_container(name),
            save_container(name),
        ]
        .iter()
        .flat_map(|base| {
            ["pak", "utoc", "ucas"].map(|extension| format!("{prefix}/{base}.{extension}"))
        })
        .filter(|relative| self.game.root.join(relative).is_file())
        .collect();
        let ue4ss = UE4SS_FOLDER.join("/");
        files.extend(
            files_below(&join(&self.game.root, UE4SS_FOLDER).join(name))
                .into_iter()
                .map(|file| format!("{ue4ss}/{name}/{file}")),
        );
        files
    }

    pub fn uninstall(&self, name: &str, force: bool) -> Result<()> {
        // The name becomes part of the paths looked at: ".." or an absolute path would reach far
        // beyond a mod
        if !is_name(name) {
            bail!(
                "{name:?} isn't a mod name: a mod name is letters and digits, starting with a \
                letter"
            );
        }
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        let manifest = files
            .manifests()?
            .into_iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, manifest)| manifest);
        let entry = record.mods.get(name).cloned();
        if manifest.is_none() && entry.is_none() {
            let named_files = self.files_named_for(name);
            if named_files.is_empty() {
                println!("{name} is not installed");
                return Ok(());
            }
            if !force {
                bail!(
                    "{name} wasn't installed by this tool (no manifest or record), so there's \
                    nothing to \
                     confirm these are its files:\n   {}\npass --force to delete them",
                    named_files.join("\n   ")
                );
            }
            for path in files.delete_unlisted(&named_files)? {
                println!("removed {path}");
            }
            return Ok(());
        }
        let mut needs = vec![];
        for (other, other_entry) in &record.mods {
            if other != name
                && other_entry
                    .requires()?
                    .iter()
                    .any(|dependency| dependency.name == name && dependency.kind.is_required())
            {
                needs.push(other.clone());
            }
        }
        if !needs.is_empty() && !force {
            let (verb, them) = if needs.len() == 1 {
                ("needs", "it")
            } else {
                ("need", "them")
            };
            bail!(
                "{} {verb} {name}: uninstall {them} first, or pass --force",
                needs.join(" and ")
            );
        }
        // Without a manifest (installed by mods.py or an older version), the record lists the files
        // but can't confirm they're unchanged
        let unverified: Vec<String> = match (&manifest, &entry) {
            (None, Some(entry)) => entry
                .files
                .iter()
                .filter(|file| self.game.root.join(file).is_file())
                .cloned()
                .collect(),
            _ => vec![],
        };
        if !unverified.is_empty() && !force {
            bail!(
                "{name} has no manifest (it was installed by mods.py or an older version), so \
                nothing \
                 confirms these are still its own files:\n   {}\npass --force to delete them (or \
                     reinstall \
                 it with this tool first, which writes its manifest)",
                unverified.join("\n   ")
            );
        }
        if entry.is_some() {
            record.mods.shift_remove(name);
            self.merge(&mut record, force)?;
        }
        let mut kept = vec![];
        if let Some(manifest) = &manifest {
            let result = files.delete_listed(&manifest.files, force)?;
            for path in &result.deleted {
                println!("removed {path}");
            }
            for path in &result.missing {
                println!("already gone: {path}");
            }
            kept = result.kept;
            let manifest_path = files.resolve(&GameFiles::manifest_path(name))?;
            std::fs::remove_file(&manifest_path)
                .with_context(|| format!("removing {}", display_path(&manifest_path)))?;
        } else {
            for path in files.delete_unlisted(&unverified)? {
                println!("removed {path}");
            }
        }
        self.save_record(&record)?;
        println!("uninstalled {name}");
        if !kept.is_empty() {
            println!(
                "kept these files, which have changed since {name} installed them (another mod or \
                you may \
                 have replaced them; --force deletes them too):\n   {}",
                kept.join("\n   ")
            );
        }
        let buildings = manifest
            .map(|manifest| manifest.new_buildings)
            .or(entry.map(|recorded| recorded.new_buildings))
            .unwrap_or_default();
        if !buildings.is_empty() {
            println!(
                "reminder: saves with a placed {} will crash without the mod; reinstall it to \
                deconstruct them",
                buildings.join(", ")
            );
        }
        Ok(())
    }

    /// What every mod in the game folder is, and whether its files are as installed: mods with
    /// manifests, mods recorded without one, the tool's merged files, and everything no mod
    /// accounts for (with what its containers replace), plus containers that replace the same
    /// asset. Mods installed from a zip are added to the install record.
    pub fn scan(&self) -> Result<()> {
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        let manifests = files.manifests()?;
        let mut owned: HashMap<String, String> = HashMap::new(); // path -> who it belongs to

        println!("== mods with manifests");
        if manifests.is_empty() {
            println!("   none");
        }
        let mut added = vec![];
        for (name, manifest) in &manifests {
            let (mut unchanged, mut changed, mut missing) = (0, vec![], vec![]);
            for file in &manifest.files {
                owned.insert(file.path.clone(), name.clone());
                match files.state(file)? {
                    State::Unchanged => unchanged += 1,
                    State::Changed => changed.push(file.path.clone()),
                    State::Missing => missing.push(file.path.clone()),
                }
            }
            owned.insert(GameFiles::manifest_path(name), name.clone());
            println!(
                "   {name} {} (installed by {}): {}{}{}",
                manifest.version,
                manifest.installed_by,
                if manifest.files.is_empty() {
                    "no files of its own (its edits are in the merged files)".to_string()
                } else {
                    format!("{unchanged} file(s) as installed")
                },
                if changed.is_empty() {
                    String::new()
                } else {
                    format!("; CHANGED since: {}", changed.join(", "))
                },
                if missing.is_empty() {
                    String::new()
                } else {
                    format!("; MISSING: {}", missing.join(", "))
                }
            );
            if !record.mods.contains_key(name) {
                record.mods.insert(
                    name.clone(),
                    Entry {
                        name: name.clone(),
                        title: manifest.title.clone(),
                        version: manifest.version.clone(),
                        game_build: manifest.game_build.clone(),
                        dependencies: manifest.dependencies.clone(),
                        files: manifest
                            .files
                            .iter()
                            .map(|file| file.path.clone())
                            .collect(),
                        tables: manifest.tables.clone(),
                        new_buildings: manifest.new_buildings.clone(),
                        ..Entry::default()
                    },
                );
                added.push(name.clone());
            }
        }
        // saved now, before the rest of the report (whose output may be cut short)
        if !added.is_empty() {
            record.load_order = load_order(&record.mods)?;
            self.save_record(&record)?;
        }

        let unmanifested: Vec<&Entry> = record
            .mods
            .values()
            .filter(|entry| !manifests.iter().any(|(owner, _)| *owner == entry.name))
            .collect();
        if !unmanifested.is_empty() {
            println!("\n== recorded without a manifest (installed by mods.py or an older version)");
            for entry in unmanifested {
                let present = entry
                    .files
                    .iter()
                    .filter(|file| self.game.root.join(file).is_file())
                    .count();
                for file in &entry.files {
                    owned.insert(file.clone(), format!("{} (recorded)", entry.name));
                }
                println!(
                    "   {} {}: {present} of {} recorded file(s) present; reinstall it with this \
                    tool to give it a manifest",
                    entry.name,
                    entry.version,
                    entry.files.len()
                );
            }
        }

        println!("\n== files the tool merges for all installed mods");
        if record.generated.is_empty() {
            // a record from before the tool kept hashes (mods.py): its merged files are the tool's,
            // and the next install or uninstall records them
            let prefix = PAK_FOLDER.join("/");
            let legacy: Vec<String> = [data_container(MERGED), save_container(MERGED_SAVE)]
                .iter()
                .flat_map(|base| {
                    ["pak", "ucas", "utoc"].map(|extension| format!("{prefix}/{base}.{extension}"))
                })
                .filter(|path| !record.mods.is_empty() && self.game.root.join(path).is_file())
                .collect();
            if legacy.is_empty() {
                println!("   none recorded");
            }
            for path in legacy {
                println!(
                    "   {path}: written without a hash (by mods.py); the next install records it"
                );
                owned.insert(path, "the tool".into());
            }
        }
        for (path, file) in &record.generated {
            owned.insert(path.clone(), "the tool".into());
            let state = match files.state(file)? {
                State::Unchanged => "as written",
                State::Changed => "CHANGED since the tool wrote it",
                State::Missing => "MISSING",
            };
            println!("   {path}: {state}");
        }
        owned.insert(
            format!("{}/{RECORD}", PAK_FOLDER.join("/")),
            "the tool".into(),
        );

        // Everything else that could be a mod: containers below Paks (not the game's own), and
        // UE4SS mods (not those UE4SS comes with)
        let paks = join(&self.game.root, &["AlchemyFactory", "Content", "Paks"]);
        let mut unknown: Vec<String> = vec![];
        for file in files_below(&paks) {
            let relative = format!("AlchemyFactory/Content/Paks/{file}");
            let game_own = !file.contains('/')
                && (file.starts_with("AlchemyFactory-Windows.") || file.starts_with("global."));
            if !game_own && !owned.contains_key(&relative) {
                unknown.push(relative);
            }
        }
        let ue4ss_mods = join(&self.game.root, UE4SS_FOLDER);
        let ue4ss_prefix = UE4SS_FOLDER.join("/");
        // a UE4SS folder (Mods/<x>, or Mods/shared/<x>) -> the files in it no manifest lists
        let mut unknown_ue4ss: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files_below(&ue4ss_mods) {
            let relative = format!("{ue4ss_prefix}/{file}");
            let top = if file.starts_with("shared/") {
                file.splitn(3, '/').take(2).collect::<Vec<_>>().join("/")
            } else {
                file.split('/').next().unwrap_or_default().to_string()
            };
            if UE4SS_OWN.contains(&top.as_str()) || owned.contains_key(&relative) {
                continue;
            }
            unknown_ue4ss
                .entry(top.clone())
                .or_default()
                .push(file[top.len()..].trim_start_matches('/').to_string());
        }
        // which mod's files are in a folder, if any
        let folder_owner = |top: &str| -> Option<String> {
            let prefix = format!("{ue4ss_prefix}/{top}/");
            owned
                .iter()
                .find(|(path, _)| path.starts_with(&prefix))
                .map(|(_, owner)| owner.clone())
        };
        println!("\n== not from any mod this tool knows");
        if unknown.is_empty() && unknown_ue4ss.is_empty() {
            println!("   nothing");
        }
        let game_packages =
            crate::unpack::package_names(&self.game.paks().join("AlchemyFactory-Windows.utoc"))
                .unwrap_or_default();
        // a container's files together: <stem> -> its extensions
        let mut containers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in &unknown {
            let (stem, extension) = path.rsplit_once('.').unwrap_or((path, ""));
            containers
                .entry(stem.to_string())
                .or_default()
                .push(extension.to_string());
        }
        for (stem, extensions) in &containers {
            // what it holds: the IoStore container's packages, else a .pak's files
            let source = ["utoc", "pak"]
                .into_iter()
                .find(|extension| extensions.iter().any(|listed| listed == extension));
            let contents = source
                .map(|extension| {
                    container_contents(&self.game.root.join(format!("{stem}.{extension}")))
                })
                .unwrap_or_default();
            let mut replaces: Vec<&String> = contents
                .iter()
                .filter(|package| game_packages.contains(*package))
                .collect();
            replaces.sort();
            let shown: Vec<&str> = replaces
                .iter()
                .take(6)
                .map(|package| package.as_str())
                .collect();
            let listed: Vec<String> = extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect();
            println!(
                "   {stem} ({}){}",
                listed.join(" "),
                if contents.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": replaces {}{}; adds {} other file(s)",
                        if shown.is_empty() {
                            "nothing".to_string()
                        } else {
                            shown.join(", ")
                        },
                        if replaces.len() > 6 { ", ..." } else { "" },
                        contents.len() - replaces.len()
                    )
                }
            );
        }
        for (top, found) in &unknown_ue4ss {
            match folder_owner(top) {
                Some(owner) => println!(
                    "   {ue4ss_prefix}/{top}/: {} file(s) {owner}'s manifest doesn't list ({})",
                    found.len(),
                    found.join(", ")
                ),
                None => println!(
                    "   {ue4ss_prefix}/{top}/ (a UE4SS mod or module, {} file(s))",
                    found.len()
                ),
            }
        }
        // A mod's zip holds paths from the game folder; extracted anywhere below it, they nest
        let misplaced: Vec<String> = unknown
            .iter()
            .filter_map(|path| {
                let below = path.strip_prefix("AlchemyFactory/Content/Paks/")?;
                below
                    .find("AlchemyFactory/")
                    .map(|at| format!("AlchemyFactory/Content/Paks/{}", &below[..at]))
            })
            .chain(
                unknown_ue4ss
                    .keys()
                    .filter(|top| *top == "AlchemyFactory")
                    .map(|_| format!("{ue4ss_prefix}/")),
            )
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        for folder in &misplaced {
            println!(
                "   note: {folder}AlchemyFactory/ looks like a mod's zip extracted into {folder} \
                 instead of the game folder. The game may still load its containers, but not \
                 merged with the other mods' edits. Delete {folder}AlchemyFactory/, then extract the zip into the game folder, the one that holds \
                 AlchemyFactory/"
            );
        }

        // Two containers with the same asset: only one copy takes effect
        let mut by_asset: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files_below(&paks) {
            if file.ends_with(".utoc") && file.contains('/') {
                for asset in container_contents(&paks.join(&file)) {
                    by_asset.entry(asset).or_default().push(file.clone());
                }
            }
        }
        let clashes: Vec<(&String, &Vec<String>)> = by_asset
            .iter()
            .filter(|(_, containers)| containers.len() > 1)
            .collect();
        println!("\n== assets in more than one mod container (only one copy takes effect)");
        if clashes.is_empty() {
            println!("   none");
        }
        for (asset, containers) in clashes {
            println!("   {asset}: {}", containers.join(", "));
        }

        if !added.is_empty() {
            println!(
                "\nadded to {RECORD} (installed from their zips): {}",
                added.join(", ")
            );
        }
        Ok(())
    }

    // --- sharing ---

    pub fn readme_for_zip(mod_: &Mod, info: &Entry) -> String {
        let mut by_kind: IndexMap<&str, Vec<String>> = IndexMap::new();
        for dependency in &mod_.requires {
            let heading = match dependency.kind {
                Kind::Required | Kind::Unordered => "Requires",
                Kind::Recommended => "Recommended",
                Kind::Optional => "Works with",
                Kind::Incompatible => "Doesn't work with",
                Kind::HiddenOptional => continue,
            };
            by_kind
                .entry(heading)
                .or_default()
                .push(requirement_text(dependency));
        }
        let mut lines = vec![format!(
            "{} {}{}",
            mod_.title,
            mod_.version,
            if mod_.author.is_empty() {
                String::new()
            } else {
                format!(" by {}", mod_.author)
            }
        )];
        if !mod_.description.is_empty() {
            lines.extend([String::new(), mod_.description.clone()]);
        }
        if !mod_.homepage.is_empty() {
            lines.push(format!("Website: {}", mod_.homepage));
        }
        if !mod_.contact.is_empty() {
            lines.push(format!("Contact: {}", mod_.contact));
        }
        lines.extend([
            String::new(),
            format!(
                "Made for Alchemy Factory build {} (Steam). After a game update it may need a new \
                version.",
                info.game_build.as_deref().unwrap_or("(unknown)")
            ),
            String::new(),
        ]);
        for heading in ["Requires", "Recommended", "Works with", "Doesn't work with"] {
            if let Some(names) = by_kind.get(heading) {
                lines.push(format!("{heading}: {}", names.join(", ")));
            }
        }
        lines.extend(["".into(), "INSTALL".into(), "".into()]);
        if info.needs_ue4ss {
            lines.extend([
                "This mod needs UE4SS: https://github.com/UE4SS-RE/RE-UE4SS/releases".to_string(),
                String::new(),
            ]);
        }
        lines.extend([
            "Extract this zip into the game's folder (Steam: right-click Alchemy Factory > Manage \
            > Browse local"
                .to_string(),
            "files), so that its AlchemyFactory folder merges with the game's. Not into ~mods: \
            the zip"
                .to_string(),
            "already holds the folders down to it. Then start the game.".to_string(),
            String::new(),
        ]);
        let needed: Vec<&str> = mod_
            .requires
            .iter()
            .filter(|dependency| dependency.kind.is_required())
            .map(|dependency| dependency.name.as_str())
            .collect();
        if !info.tables.is_empty() {
            lines.push("It replaces these game tables:".to_string());
            lines.extend(info.tables.iter().map(|table| format!("   {table}")));
            lines.extend([
                "Another mod that replaces one of them can't be used at the same time, unless \
                both are"
                    .to_string(),
                "installed with a tool that merges them.".to_string(),
                String::new(),
            ]);
        }
        if !info.save_edits.is_empty() {
            lines.extend([
                "It changes the save that new games start from, so it only affects games started \
                with it"
                    .to_string(),
                "installed. Another mod that does the same can't be used at the same time, unless \
                both are"
                    .to_string(),
                "installed with a tool that merges them.".to_string(),
                String::new(),
            ]);
        }
        if !info.tables.is_empty() && !needed.is_empty() {
            lines.push(format!(
                "Its tables already include the table changes of {}. After installing",
                needed.join(", ")
            ));
            lines.push(
                "those, delete their own table files, or they replace this mod's:".to_string(),
            );
            for name in &needed {
                lines.push(format!(
                    "   AlchemyFactory/Content/Paks/~mods/{}.pak, .ucas and .utoc",
                    data_container(name)
                ));
            }
            lines.extend(["Keep their other files.".to_string(), String::new()]);
        }
        lines.push("It adds these files:".to_string());
        lines.extend(
            info.data_files
                .iter()
                .chain(&info.files)
                .map(|file| format!("   {file}")),
        );
        lines.extend(["".into(), "UNINSTALL".into(), "".into()]);
        if !info.new_buildings.is_empty() {
            lines.push(format!(
                "First, in every save you want to keep, deconstruct every {}.",
                info.new_buildings.join(", ")
            ));
            lines.extend([
                "A save with one of these placed crashes the game once the mod is removed."
                    .to_string(),
                String::new(),
            ]);
        }
        lines.extend([
            "Delete the files listed above, and this one:".to_string(),
            format!(
                "   AlchemyFactory/Content/Paks/~mods/manifests/{}.json",
                mod_.name
            ),
            format!("Or run: alchemists-tools mod uninstall {}", mod_.name),
            String::new(),
        ]);
        lines.join("\r\n")
    }

    pub fn dist(&self, mod_: &Mod, build_folder: &Path, info: &Entry) -> Result<PathBuf> {
        use std::io::Write;
        let path = self
            .build
            .join(format!("{}_{}.zip", mod_.name, mod_.version));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut listed = vec![];
        for relative in info.data_files.iter().chain(&info.files) {
            zip.start_file(relative, options)?;
            let data = std::fs::read(build_folder.join(relative))?;
            zip.write_all(&data)?;
            let (size, sha256) = hash(&build_folder.join(relative))?;
            listed.push(FileRecord {
                path: relative.clone(),
                size,
                sha256,
            });
        }
        // The mod names itself and its files, so a tool can tell it apart and remove it exactly
        let manifest = Self::manifest_for(info, "zip", listed);
        zip.start_file(GameFiles::manifest_path(&mod_.name), options)?;
        zip.write_all((serde_json::to_string_pretty(&manifest)? + "\n").as_bytes())?;
        zip.finish()?;
        println!("zipped -> {}", display_path(&path));
        // Install instructions and the description go beside the zip, not in it: mod sites show
        // them on the mod's page
        let readme = self
            .build
            .join(format!("{}_{}_README.txt", mod_.name, mod_.version));
        std::fs::write(&readme, Self::readme_for_zip(mod_, info))?;
        println!("README -> {}", display_path(&readme));
        Ok(path)
    }

    pub fn new_mod(&self, name: &str) -> Result<()> {
        if !is_name(name) || name == MERGED || name == MERGED_SAVE {
            bail!("a mod name is letters and digits, starting with a letter");
        }
        let folder = self.mods.join(name);
        if folder.exists() {
            bail!("{} already exists", display_path(&folder));
        }
        std::fs::create_dir_all(folder.join("edits"))?;
        write_json(
            &folder.join("mod.json"),
            &json!({"name": name, "title": name, "version": "0.1.0", "description": "",
                    "author": "", "homepage": "",
                    "dependencies": [], "content": [], "craft-types": {}, "new-craft-types": []}),
        )?;
        // every table in it starts with `_`, so it changes nothing until one is renamed
        std::fs::write(folder.join("edits").join("example.json"), EXAMPLE_EDITS)?;
        println!(
            "made {}: put table edits in edits/ (edits/example.json shows how), a UE4SS mod in \
             ue4ss/, and list cooked content (in /Game/Mods/{name}/ in the stub project) in \
             mod.json",
            display_path(&folder)
        );
        Ok(())
    }

    pub fn list(&self) -> Result<()> {
        let record = self.record()?;
        let mut local = vec![];
        let mut folders: Vec<PathBuf> = std::fs::read_dir(&self.mods)
            .into_iter()
            .flatten()
            .flatten()
            .map(|folder_entry| folder_entry.path())
            .filter(|path| path.join("mod.json").exists())
            .collect();
        folders.sort();
        for folder in folders {
            let mod_ = Mod::load(&folder)?;
            let state = match record.mods.get(&mod_.name) {
                Some(entry) => format!("installed ({})", entry.version),
                None => "not installed".to_string(),
            };
            println!("{} {}: {state}", mod_.name, mod_.version);
            local.push(mod_.name);
        }
        for (name, entry) in &record.mods {
            if !local.contains(name) {
                println!(
                    "{name} {}: installed (not in the mods folder)",
                    entry.version
                );
            }
        }
        if !record.mods.is_empty() {
            println!("load order: {}", record.load_order.join(", "));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependencies() {
        let dependency = parse_dependency("VaultCore >= 0.2.0", "test").unwrap();
        assert_eq!(
            (
                dependency.kind,
                dependency.name.as_str(),
                dependency.version
            ),
            (Kind::Required, "VaultCore", Some((0, 2, 0)))
        );
        assert!(satisfies((0, 2, 1), &dependency) && !satisfies((0, 1, 9), &dependency));
        for (text, kind) in [
            ("? A", Kind::Optional),
            ("(?) A", Kind::HiddenOptional),
            ("+ A", Kind::Recommended),
            ("~ A", Kind::Unordered),
            ("! A", Kind::Incompatible),
        ] {
            assert_eq!(parse_dependency(text, "test").unwrap().kind, kind, "{text}");
        }
        assert!(parse_dependency("A >= 1.2", "test").is_err());
        assert!(parse_dependency("1A", "test").is_err());
    }

    #[test]
    fn lua_requires() {
        let script = "local L = require(\"AlchemistsLibrary\")\nlocal U = require 'UEHelpers'";
        assert_eq!(required_modules(script), ["AlchemistsLibrary", "UEHelpers"]);
    }

    #[test]
    fn order_follows_dependencies() {
        let entry = |name: &str, dependencies: &[&str]| Entry {
            name: name.into(),
            version: "0.1.0".into(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
            ..Entry::default()
        };
        let mods: IndexMap<String, Entry> =
            [entry("B", &["A"]), entry("A", &[]), entry("C", &["~ B"])]
                .into_iter()
                .map(|entry| (entry.name.clone(), entry))
                .collect();
        assert_eq!(load_order(&mods).unwrap(), ["A", "B", "C"]);
        let looped: IndexMap<String, Entry> = [entry("A", &["B"]), entry("B", &["A"])]
            .into_iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect();
        assert!(load_order(&looped)
            .unwrap_err()
            .to_string()
            .contains("dependency loop"));
    }
}
