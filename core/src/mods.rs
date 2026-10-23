//! Mod folders: build, install and share mods kept as folders.
//!
//! A mod folder holds:
//! - `mod.json`: name, title, version, dependencies, the game build it's made for, the game functions
//!   its C++ detours, cooked content to package, craft types;
//! - `thumbnail.png`: its picture in the GUI;
//! - `edits/*.json`: table edits ([`crate::edits`]); `edits/if-<Mod>/`: applied only when
//!   `<Mod>` is installed too;
//! - `save/DefaultGame.json`: edits to the template save new games start from ([`crate::save`]);
//! - `ue4ss/`: a UE4SS mod, Lua (`Scripts/main.lua`) or C++ (`dlls/main.dll`) -> `ue4ss/Mods/<name>/`;
//! - `ue4ss-shared/<name>X/`: Lua modules other mods `require` -> `ue4ss/Mods/shared/<name>X/`;
//!
//! plus assets cooked in the stub project (`/Game/Mods/<name>/`), packed as `zzz_<name>Content_P`.
//!
//! [`Workspace::apply`] makes the installed mods exactly a given set: everything is built and checked
//! first, then the game folder is changed in one [`crate::files::Transaction`].
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
use crate::files::{hash, FileRecord, GameFiles, Manifest, State, Transaction};
use crate::game::{display_path, join, Game};
use crate::package::{check_container, script_objects, write_container};
use crate::report::{self, note, warning};
use crate::save::{self, SaveEdit};
use crate::schema::Schemas;
use crate::text::Localizer;
use crate::unpack::Assets;

const STEAM_APP: &str = "3669570";
pub(crate) const PAK_FOLDER: &[&str] = &["AlchemyFactory", "Content", "Paks", "~mods"];
pub(crate) const UE4SS_FOLDER: &[&str] = &["AlchemyFactory", "Binaries", "Win64", "ue4ss", "Mods"];
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
    "team",
    "dependencies",
    "game-build",
    "detours",
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

pub(crate) fn version_text(version: Version) -> String {
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
    pub fn is_required(self) -> bool {
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

pub(crate) fn is_name(text: &str) -> bool {
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

pub fn satisfies(version: Version, dependency: &Dependency) -> bool {
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
pub fn requirement_text(dependency: &Dependency) -> String {
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
    /// Game functions its C++ code detours (as its mod.json declares): no two mods may detour one.
    pub detours: Vec<String>,
    /// Game functions it hooks through UE4SS, as found in its Lua (`RegisterHook`) and DLL.
    pub hooks: Vec<String>,
    /// What couldn't be checked about it, warned about whenever it's installed (an imported mod's:
    /// see [`crate::import`]).
    pub cautions: Vec<String>,
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
    /// The Thunderstore team that publishes it (see [`crate::sharing`]).
    pub team: String,
    pub dependencies: Vec<String>,
    pub requires: Vec<Dependency>,
    /// The game's Steam build it's made for, if mod.json says.
    pub game_build: Option<String>,
    /// The game functions its C++ code detours, if mod.json says (None: it doesn't say).
    pub detours: Option<Vec<String>>,
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
        let game_build = match fields.get("game-build") {
            None => None,
            Some(Value::String(build)) if is_build(build) => Some(build.clone()),
            Some(other) => bail!(
                "{at}: game-build {other} must be the game's Steam build number, as a string (e.g. \
                 \"25991404\"; alchemists-tools check shows the installed one)"
            ),
        };
        let detours = match fields.get("detours") {
            None => None,
            listed => Some(string_list(listed, &at, "detours")?),
        };
        let team = text("team", "");
        if !team.is_empty() && !crate::sharing::is_team(&team) {
            bail!(
                "{at}: team {team:?} must be a Thunderstore team name: letters, digits and _, not \
                 starting or ending with _"
            );
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
            team,
            name,
            version,
            dependencies,
            requires,
            game_build,
            detours,
            content,
            craft_types,
            new_craft_types,
            edits,
            optional_edits,
            save_edits,
        })
    }
}

impl Mod {
    /// Its picture: `thumbnail.png` (or `.jpg`) in its folder.
    pub fn thumbnail(&self) -> Option<PathBuf> {
        ["thumbnail.png", "thumbnail.jpg"]
            .iter()
            .map(|file| self.folder.join(file))
            .find(|path| path.is_file())
    }
}

fn is_build(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether a mod made for game build `made_for` is older than the game's build `current`.
pub fn older_build(made_for: &str, current: &str) -> bool {
    match (made_for.parse::<u64>(), current.parse::<u64>()) {
        (Ok(made_for), Ok(current)) => made_for < current,
        _ => false,
    }
}

/// The install record's entry for a mod known only from its manifest (one installed from its zip).
fn entry_from_manifest(name: &str, manifest: &Manifest) -> Entry {
    Entry {
        name: name.to_string(),
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
        detours: manifest.detours.clone(),
        hooks: manifest.hooks.clone(),
        cautions: manifest.cautions.clone(),
        ..Entry::default()
    }
}

/// The install record of a game folder: the mods installed in it (empty if none are).
pub fn installed_record(game: &Game) -> Result<Record> {
    let path = join(&game.root, PAK_FOLDER).join(RECORD);
    if !path.exists() {
        return Ok(Record::default());
    }
    Ok(serde_json::from_value(read_json(&path)?)?)
}

/// Stop if the game is running: it holds the files mods replace.
pub(crate) fn check_game_closed() -> Result<()> {
    if crate::environment::game_running() {
        bail!("the game is running: close it first (it holds the files mods replace)");
    }
    Ok(())
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
    /// The folders of mod folders. A mod in more than one is taken from the first; new mods go in
    /// the first.
    pub mods: Vec<PathBuf>,
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
        mods: Vec<PathBuf>,
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

    pub(crate) fn schemas(&self) -> Result<&Schemas> {
        if !self.usmap.is_file() {
            bail!(
                "no .usmap mappings{}: dump them with UE4SS (Ctrl + Numpad 6 in a new save), or \
                 give the file in the settings or with --usmap",
                if self.usmap.as_os_str().is_empty() {
                    " found".to_string()
                } else {
                    format!(" at {}", display_path(&self.usmap))
                }
            );
        }
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

    pub(crate) fn game_for_edits(&self) -> Result<edits::Game<'_>> {
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

    /// The first folder of mods, which gets new mods (and the tool's Lua library, if no folder has
    /// it).
    fn first_mods(&self) -> PathBuf {
        self.mods
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("mods"))
    }

    /// The folder of the mod `name`, from the first folder of mods that has it.
    pub fn local_folder(&self, name: &str) -> Option<PathBuf> {
        self.mod_folders().into_iter().find(|folder| {
            folder
                .file_name()
                .is_some_and(|folder_name| folder_name == name)
        })
    }

    /// Every mod folder (one with a mod.json) in the folders of mods, in the order they're taken:
    /// each folder's own mod folders first, then those in the git checkouts it holds
    /// (`<checkout>/<mod>/` or `<checkout>/mods/<mod>/`, as `mod add` clones them). A name can come
    /// more than once.
    fn every_mod_folder(&self) -> Vec<PathBuf> {
        mod_folders_in(&self.mods)
    }

    /// Every mod folder in the folders of mods, sorted by name; for a name in more than one, the
    /// one taken first (see [`Self::every_mod_folder`]).
    pub fn mod_folders(&self) -> Vec<PathBuf> {
        let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
        for folder in self.every_mod_folder() {
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            found.entry(name).or_insert(folder);
        }
        found.into_values().collect()
    }

    /// The git checkout a mod folder is in, if it's one `mod add` cloned (a checkout directly in a
    /// folder of mods), and the URL it came from.
    pub fn git_checkout(&self, folder: &Path) -> Option<(PathBuf, String)> {
        folder
            .ancestors()
            .take(3)
            .find(|checkout| {
                checkout.join(".git").exists()
                    && self
                        .mods
                        .iter()
                        .any(|mods| checkout.parent() == Some(mods.as_path()))
            })
            .map(|checkout| {
                let url = crate::git::origin(checkout).unwrap_or_default();
                (checkout.to_path_buf(), url)
            })
    }

    /// Clone a git repository into the first folder of mods, so the mods it holds are listed
    /// (installing them is up to `apply` or `install`); returns their names.
    pub fn add_from_git(&self, url: &str, reference: Option<&str>) -> Result<Vec<String>> {
        let Some(mods) = self.mods.first() else {
            bail!(
                "no mods folder is set: set one first (alchemists-tools settings --mods <folder>)"
            );
        };
        std::fs::create_dir_all(mods)?;
        let mut folder = mods.join(crate::git::repository_name(url)?);
        if folder.exists() {
            bail!(
                "{} already exists: to update it, alchemists-tools mod update <name>",
                display_path(&folder)
            );
        }
        note!("cloning {url} into {}", display_path(&folder));
        crate::git::clone(url, reference, &folder)?;
        // a repository that is one mod is named after it, as a mod's folder must be
        if folder.join("mod.json").is_file() {
            let found = Mod::load(&folder);
            let wanted = found.as_ref().ok().map(|found| mods.join(&found.name));
            if let Some(wanted) = wanted.filter(|wanted| *wanted != folder) {
                if wanted.exists() {
                    let _ = std::fs::remove_dir_all(&folder);
                    bail!(
                        "the repository's mod is named after {}, which already exists",
                        display_path(&wanted)
                    );
                }
                std::fs::rename(&folder, &wanted)?;
                folder = wanted;
            }
        }
        let names: Vec<String> = self
            .every_mod_folder()
            .into_iter()
            .filter(|found| found.starts_with(&folder))
            .map(|found| found.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        if names.is_empty() {
            let _ = std::fs::remove_dir_all(&folder);
            bail!(
                "{url} holds no mod: no mod.json at its top, in a folder at its top, or in mods/"
            );
        }
        note!(
            "added {} from {url}: tick to install (or alchemists-tools mod install <name>)",
            names.join(", ")
        );
        Ok(names)
    }

    /// Pull the git repository a mod was added from; the next install builds the new version.
    pub fn update_from_git(&self, name: &str) -> Result<()> {
        let folder = self
            .local_folder(name)
            .with_context(|| format!("there's no mod {name} in the mods folders"))?;
        let Some((checkout, url)) = self.git_checkout(&folder) else {
            bail!("{name} wasn't added from git (its folder isn't a clone in a mods folder)");
        };
        note!("updating {} from {url}", display_path(&checkout));
        let told = crate::git::pull(&checkout)?;
        note!("{}", told.trim());
        Ok(())
    }

    /// The mods kept as zips in the folders of mods, by name: for a name in more than one folder,
    /// the first folder's; in one folder, the newest version.
    pub fn archives(&self) -> BTreeMap<String, Archive> {
        let mut found: BTreeMap<String, Archive> = BTreeMap::new();
        for mods in &self.mods {
            let mut newest: BTreeMap<String, Archive> = BTreeMap::new();
            for entry in std::fs::read_dir(mods).into_iter().flatten().flatten() {
                let path = entry.path();
                if !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
                {
                    continue;
                }
                let Some(archive) = read_archive(&path) else {
                    continue;
                };
                let name = archive.manifest.name.clone();
                let version = |archive: &Archive| parse_version(&archive.manifest.version, "").ok();
                let newer = newest
                    .get(&name)
                    .is_none_or(|other| version(&archive) > version(other));
                if is_name(&name) && newer {
                    newest.insert(name, archive);
                }
            }
            for (name, archive) in newest {
                found.entry(name).or_insert(archive);
            }
        }
        found
    }

    /// Before an installed mod that no folder of mods holds is uninstalled, keep it as a zip in the
    /// first folder of mods, so it stays listed and can be installed again: its files, its manifest,
    /// and its install record entry (which holds the table edits merged for it). Nothing is kept if
    /// a zip of it is there already. A missing file stops this, unless `force`.
    fn keep_copy(
        &self,
        files: &GameFiles,
        entry: &Entry,
        manifest: Option<&Manifest>,
        force: bool,
    ) -> Result<()> {
        let name = &entry.name;
        if self.archives().contains_key(name) {
            return Ok(());
        }
        let Some(mods) = self.mods.first() else {
            if force {
                warning!("no mods folder is set, so no copy of {name} is kept");
                return Ok(());
            }
            bail!(
                "{name} isn't in a mods folder, and no mods folder is set to keep a copy of it in: \
                 set one, or force uninstalling it without a copy"
            );
        };
        let listed: Vec<String> = match manifest {
            Some(manifest) => manifest
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            None => entry.files.clone(),
        };
        let missing: Vec<&String> = listed
            .iter()
            .filter(|path| !files.resolve(path).is_ok_and(|full| full.is_file()))
            .collect();
        if !missing.is_empty() {
            let missing: Vec<&str> = missing.iter().map(|path| path.as_str()).collect();
            if force {
                warning!(
                    "{name}'s files aren't all there ({}), so no copy of it is kept",
                    missing.join(", ")
                );
                return Ok(());
            }
            bail!(
                "{name} isn't in a mods folder, and a copy can't be kept to install it again, as \
                 these of its files are missing: {}; force uninstalling it without a copy",
                missing.join(", ")
            );
        }
        let mut kept = match manifest {
            Some(manifest) => manifest.clone(),
            None => Self::manifest_for(entry, "alchemists-tools", vec![]),
        };
        kept.files = listed
            .iter()
            .map(|path| files.record(path))
            .collect::<Result<_>>()?;
        std::fs::create_dir_all(mods)?;
        let path = mods.join(format!("{name}_{}.zip", entry.version));
        let mut contents = vec![];
        for file in &kept.files {
            contents.push((
                file.path.clone(),
                std::fs::read(files.resolve(&file.path)?)?,
            ));
        }
        write_mod_zip(&path, &kept, entry, contents)?;
        note!(
            "kept a copy of {name} as {}, to install it again",
            display_path(&path)
        );
        Ok(())
    }

    /// Where the tool's own Lua library mod goes: the folder of mods that has it, else the first.
    fn library_home(&self) -> PathBuf {
        self.local_folder("AlchemistsLibrary")
            .and_then(|folder| folder.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.first_mods())
    }

    fn mod_folder(&self, name: &str) -> PathBuf {
        let given = Path::new(name);
        if given.join("mod.json").exists() {
            given.to_path_buf()
        } else {
            self.local_folder(name)
                .unwrap_or_else(|| self.first_mods().join(name))
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
        installed_record(&self.game)
    }

    /// Write the install record, as part of a change to the game folder.
    fn save_record(&self, transaction: &mut Transaction, record: &Record) -> Result<()> {
        let relative = format!("{}/{RECORD}", PAK_FOLDER.join("/"));
        transaction.write(
            &relative,
            (serde_json::to_string_pretty(record)? + "\n").as_bytes(),
        )
    }

    // --- craft types ---

    /// The `allocations.json` beside the mod `name` (in its folder of mods).
    fn allocations_path(&self, name: &str) -> PathBuf {
        self.local_folder(name)
            .and_then(|folder| folder.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.first_mods())
            .join("allocations.json")
    }

    fn allocations_in(path: &Path) -> Result<IndexMap<String, u8>> {
        if !path.exists() {
            return Ok(IndexMap::new());
        }
        let value = read_json(path)?;
        Ok(serde_json::from_value(
            value.get("craft-types").cloned().unwrap_or(json!({})),
        )?)
    }

    /// Every folder of mods' craft type numbers.
    fn allocations(&self) -> Result<IndexMap<String, u8>> {
        let mut numbers = IndexMap::new();
        for mods in &self.mods {
            numbers.extend(Self::allocations_in(&mods.join("allocations.json"))?);
        }
        Ok(numbers)
    }

    /// Numbers for the mod's new craft types, kept in `allocations.json` beside it so they never
    /// change once given.
    fn allocate_craft_types(
        &self,
        name: &str,
        new_craft_types: &[String],
    ) -> Result<IndexMap<String, u8>> {
        let mut numbers = self.allocations()?;
        let path = self.allocations_path(name);
        let mut own = Self::allocations_in(&path)?;
        let mut changed = false;
        for craft_type in new_craft_types {
            let key = format!("{name}:{craft_type}");
            if !numbers.contains_key(&key) {
                let used: HashSet<u8> = numbers.values().copied().collect();
                let free = (FIRST_CRAFT_TYPE..=255)
                    .find(|number| !used.contains(number))
                    .context("no craft type numbers left (26-255)")?;
                numbers.insert(key.clone(), free);
                own.insert(key.clone(), free);
                changed = true;
                note!("   new craft type {key} = {free}");
            }
        }
        if changed {
            write_json(&path, &json!({"craft-types": own}))?;
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
    pub(crate) fn find_dependency(&self, dependency: &Dependency, at: &str) -> Result<Entry> {
        // the tool carries its own Lua library: written when missing, updated when older
        if dependency.name == "AlchemistsLibrary" {
            crate::probe::write_library(&self.library_home())?;
        }
        let entry = if let Some(local) = self.local_folder(&dependency.name) {
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
pub(crate) const UE4SS_OWN: &[&str] = &[
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

/// Which mod's files are in the UE4SS folder `Mods/<top>/`, if any.
fn folder_owner(owned: &HashMap<String, String>, top: &str) -> Option<String> {
    let prefix = format!("{}/{top}/", UE4SS_FOLDER.join("/"));
    owned
        .iter()
        .find(|(path, _)| path.starts_with(&prefix))
        .map(|(_, owner)| owner.clone())
}

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
    string_arguments(script, "require")
}

/// The text each call of `function` in a Lua script is given first, when that's a string:
/// `function("text", ...)` or `function "text"`.
pub(crate) fn string_arguments(script: &str, function: &str) -> Vec<String> {
    let mut modules = vec![];
    let mut remaining = script;
    while let Some(at) = remaining.find(function) {
        remaining = &remaining[at + function.len()..];
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

/// The reflected functions (`/Script/Module.Class:Function`) a DLL names, as UTF-16 (C++'s
/// `STR("...")`) or as bytes.
pub(crate) fn function_paths(dll: &[u8]) -> Vec<String> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"/Script/[A-Za-z0-9_]+\.[A-Za-z0-9_]+:[A-Za-z0-9_]+").unwrap()
    });
    let mut texts = vec![String::from_utf8_lossy(dll).into_owned()];
    for start in 0..2 {
        let units: Vec<u16> = dll[start.min(dll.len())..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        texts.push(String::from_utf16_lossy(&units));
    }
    let found: BTreeSet<String> = texts
        .iter()
        .flat_map(|text| {
            pattern
                .find_iter(text)
                .map(|found| found.as_str().to_string())
        })
        .collect();
    found.into_iter().collect()
}

/// The engine's own functions, which many mods hook (to start when a player spawns, say), so
/// sharing one isn't worth a warning.
fn is_engine_function(path: &str) -> bool {
    ["/Script/Engine.", "/Script/CoreUObject.", "/Script/UMG."]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// `A`, `A and B`, `A, B and C`.
fn listed(names: &[&String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.to_string(),
        [rest @ .., last] => format!(
            "{} and {last}",
            rest.iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

impl Workspace {
    /// Build a mod into `<build>/<name>/`: its tables (on top of the mods it requires), its cooked
    /// content, its template save, and its UE4SS files; then check them against each other. Its
    /// warnings are reported as about it.
    pub fn build_mod(&self, mod_: &Mod) -> Result<(PathBuf, Entry)> {
        report::about(&mod_.name, || self.build(mod_))
    }

    fn build(&self, mod_: &Mod) -> Result<(PathBuf, Entry)> {
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
        // the game's layouts (the .usmap) are only read when the mod needs them: for its tables,
        // craft types or cooked content, not for a mod of only Lua or C++
        let mut machine_types: IndexMap<String, u8> = IndexMap::new();
        for (asset, value) in &mod_.craft_types {
            let value =
                craft_type_number(value, mod_, &known, &format!("{name}/mod.json craft-types"))?;
            let text = match &value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            machine_types.insert(
                asset.clone(),
                content::craft_type_number(&text, self.schemas()?)?,
            );
        }
        let current_build = game_build(&self.game);
        if let (Some(made_for), Some(current)) = (&mod_.game_build, &current_build) {
            if older_build(made_for, current) {
                warning!(
                    "made for game build {made_for}, and the game is build {current} now: check \
                     it still works, then set \"game-build\" in mod.json to {current}"
                );
            }
        }
        let mut info = Entry {
            name: name.clone(),
            title: mod_.title.clone(),
            version: mod_.version.clone(),
            game_build: current_build,
            dependencies: mod_.dependencies.clone(),
            detours: mod_.detours.clone().unwrap_or_default(),
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
            note!(
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
            note!(
                ".pak mod: {rows} row(s) in {} table(s), checked",
                result.changed.len()
            );
            for line in &result.report {
                note!("   {line}");
            }
            for warning in &result.warnings {
                warning!("{warning}");
            }
            info.tables = result.changed.keys().cloned().collect();
            info.tables.sort();
            if !optional.is_empty() {
                let others: Vec<&str> = optional.keys().map(|name| name.as_str()).collect();
                note!(
                    "   edits for when {} are installed are checked when they're merged",
                    others.join(", ")
                );
            }
            built = Some(result);
        }

        if !mod_.content.is_empty() {
            note!("== cooked content ({})", mod_.content.join(", "));
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
                note!("   craft type {old} -> {value} in {asset}");
            }
            let utoc = paks.join(format!("{}.utoc", content_container(name)));
            let packaged =
                content::package(&utoc, &staged, &self.game, self.schemas()?, &machine_types)?;
            for (package, components) in &packaged.components {
                note!("   {package}:");
                for component in components {
                    note!("      {}", content::describe(component));
                }
            }
            info.assets = packaged.assets;
        }

        if !mod_.save_edits.is_empty() {
            note!("== template save (save/DefaultGame.json)");
            let edits: Vec<SaveEdit> = needed
                .iter()
                .flat_map(|entry| entry.save_edits.clone())
                .chain(mod_.save_edits.clone())
                .collect();
            self.write_save(&edits, &paks, &save_container(name))?;
        }

        let scripts = mod_.folder.join("ue4ss");
        if scripts.is_dir() {
            note!("== UE4SS mod ({})", display_path(&scripts));
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
            note!("== UE4SS shared Lua modules ({})", display_path(&shared));
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
        for folder in self.mod_folders() {
            for module in std::fs::read_dir(folder.join("ue4ss-shared"))
                .into_iter()
                .flatten()
                .flatten()
            {
                libraries.insert(
                    module.file_name().to_string_lossy().into_owned(),
                    folder.file_name().unwrap().to_string_lossy().into_owned(),
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
        // and the game functions it hooks: Lua's RegisterHook, and the functions its DLL names
        let mut hooks: BTreeSet<String> = BTreeSet::new();
        for folder in [&scripts, &shared] {
            for file in files_below(folder)
                .into_iter()
                .filter(|file| file.ends_with(".lua"))
            {
                let text = std::fs::read_to_string(folder.join(&file)).unwrap_or_default();
                hooks.extend(string_arguments(&text, "RegisterHook"));
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
        let dll = scripts.join("dlls").join("main.dll");
        if dll.is_file() {
            let named = function_paths(&std::fs::read(&dll)?);
            if !named.is_empty() && mod_.detours.is_none() {
                warning!(
                    "dlls/main.dll names {} game function(s) ({}{}); list the ones its code \
                     detours in mod.json's \"detours\" (or [] for none), so that installing \
                     stops when another mod detours one too",
                    named.len(),
                    named.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                    if named.len() > 3 { ", ..." } else { "" }
                );
            }
            hooks.extend(named);
        }
        for detour in &info.detours {
            hooks.remove(detour);
        }
        info.hooks = hooks.into_iter().collect();

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
        let named: HashSet<u8> = if built.is_some() || !machine_types.is_empty() {
            self.schemas()?
                .enums
                .get("EBeltTDCraftType")
                .map(|values| values.keys().filter_map(|key| key.parse().ok()).collect())
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
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
                        warning!(
                            "recipe {row} has craft type {number}, which no machine \
                            in this mod or the mods it requires runs"
                        );
                    }
                }
            }
        }
        for number in machine_types.values() {
            if !named.contains(number) && !recipe_types.contains(number) {
                warning!(
                    "craft type {number} has no recipes in this mod, so its \
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
        note!(
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
            note!(
                "   template save: {}{place} {old} -> {}",
                edit.property,
                edit.value
            );
        }
        std::fs::create_dir_all(folder)?;
        save::write_template_pak(folder, base, &edited, &self.game)
    }

    // --- merging installed mods ---

    fn report_clashes(&self, order: &[String], mods: &IndexMap<String, Entry>) -> Result<()> {
        // the game's tables (and the .usmap to read them) only if a mod edits tables
        let edits_tables = order
            .iter()
            .any(|name| !mods[name].edits.is_empty() || !mods[name].optional_edits.is_empty());
        let game_tables = if edits_tables {
            Some((self.assets()?, self.schemas()?))
        } else {
            None
        };
        let mut game_rows: HashMap<String, HashSet<String>> = HashMap::new();
        let mut is_game_row = |table: &str, row: &str| -> bool {
            let Some((assets, schemas)) = game_tables else {
                return false;
            };
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
                                problems.push((
                                    vec![first.clone(), name.clone()],
                                    format!("{first} and {name} both {what} {table}.{}", key.2),
                                ));
                            }
                        }
                        let mut values_set = vec![];
                        leaf_values(fields, vec![table.clone(), row.clone()], &mut values_set);
                        for (path, value) in values_set {
                            if let Some((first, old)) = set_by.get(&path) {
                                if first != name && *old != value && !related[name].contains(first)
                                {
                                    report::warning_about(
                                        &[first.clone(), name.clone()],
                                        format!(
                                            "{first} and {name} both set {}; {name}'s value \
                                        is used (it loads later). \
                                         Make one depend on the other to decide.",
                                            path.join(".")
                                        ),
                                    );
                                }
                            }
                            set_by.insert(path, (name.clone(), value));
                        }
                    }
                }
            }
        }
        // Game functions: two detours of one function can't both work, and a hook beside a
        // detour, or another mod's hook, may not see what it expects
        let mut detoured_by: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
        let mut hooked_by: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
        for name in order {
            for function in &mods[name].detours {
                detoured_by.entry(function).or_default().push(name);
            }
            for function in mods[name]
                .hooks
                .iter()
                .filter(|function| !is_engine_function(function))
            {
                hooked_by.entry(function).or_default().push(name);
            }
        }
        for (function, names) in &detoured_by {
            if names.len() > 1 {
                problems.push((
                    names.iter().map(|name| name.to_string()).collect(),
                    format!(
                        "{} each detour {function}, and only one mod can replace a game \
                         function's code",
                        listed(names)
                    ),
                ));
            }
            let hooking: Vec<&String> = hooked_by
                .get(function)
                .into_iter()
                .flatten()
                .filter(|name| !names.contains(name))
                .copied()
                .collect();
            if !hooking.is_empty() {
                report::warning_about(
                    &names
                        .iter()
                        .chain(&hooking)
                        .map(|name| name.to_string())
                        .collect::<Vec<_>>(),
                    format!(
                        "{} {} {function}, which {} {}: check the hooks still see what they \
                         expect",
                        listed(&hooking),
                        if hooking.len() == 1 { "hooks" } else { "hook" },
                        listed(names),
                        if names.len() == 1 {
                            "detours"
                        } else {
                            "detour"
                        }
                    ),
                );
            }
        }
        for (function, names) in &hooked_by {
            let unrelated = names.iter().any(|name| {
                names.iter().any(|other| {
                    other != name
                        && !related[*name].contains(*other)
                        && !related[*other].contains(*name)
                })
            });
            if unrelated && !detoured_by.contains_key(function) {
                report::warning_about(
                    &names
                        .iter()
                        .map(|name| name.to_string())
                        .collect::<Vec<_>>(),
                    format!(
                        "{} each hook {function}: check they don't undo each other's changes",
                        listed(names)
                    ),
                );
            }
        }
        if !problems.is_empty() {
            return Err(fail("mods clash", problems));
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
                        report::warning_about(
                            &[first.clone(), name.clone()],
                            format!(
                                "{first} and {name} both change {} in the template save; \
                                {name}'s value is used (it loads later)",
                                edit.property
                            ),
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
        note!(
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
                warning!("{relative} {problem}; replacing it anyway (--force)");
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
        transaction: &mut Transaction,
        record: &mut Record,
        base: &str,
        work: Option<&Path>,
    ) -> Result<bool> {
        let prefix = PAK_FOLDER.join("/");
        let mut removed = false;
        for extension in ["pak", "utoc", "ucas"] {
            let relative = format!("{prefix}/{base}.{extension}");
            removed |= transaction.delete(&relative)?;
            record.generated.shift_remove(&relative);
        }
        if let Some(work) = work {
            for file in files_below(work) {
                let relative = format!("{prefix}/{file}");
                transaction.copy(&work.join(&file), &relative)?;
                record
                    .generated
                    .insert(relative.clone(), transaction.files().record(&relative)?);
            }
        }
        Ok(removed)
    }

    /// Rebuild `zzz_ModTables_P` (and `zzz_ModSave_P`) from every installed mod's edits, as part of
    /// a change to the game folder.
    fn merge(&self, record: &mut Record, force: bool, transaction: &mut Transaction) -> Result<()> {
        let order = load_order(&record.mods)?;
        for name in &order {
            for caution in &record.mods[name].cautions {
                report::warning_about(std::slice::from_ref(name), caution.clone());
            }
        }
        self.report_clashes(&order, &record.mods)?;
        let present: HashSet<String> = order.iter().cloned().collect();
        let sources: Vec<edits::Source> = order
            .iter()
            .flat_map(|name| table_edits_of(&record.mods[name], &present))
            .collect();
        // which mods edit each row, to say which mods a warning about one is about
        let mut row_editors: HashMap<(String, String), Vec<String>> = HashMap::new();
        for name in &order {
            for source in table_edits_of(&record.mods[name], &present) {
                for (table, rows) in source.contents.as_object().into_iter().flatten() {
                    for row in rows.as_object().into_iter().flatten().map(|(row, _)| row) {
                        let editors = row_editors.entry((table.clone(), row.clone())).or_default();
                        if !editors.contains(name) {
                            editors.push(name.clone());
                        }
                    }
                }
            }
        }
        record.load_order = order.clone();
        let files = transaction.files();
        let base = data_container(MERGED);
        let save_base = save_container(MERGED_SAVE);
        // every merged file is checked before any is touched, and both are built before either
        // is replaced, so a refusal or a failed build leaves the game folder as it was
        self.check_generated(files, record, &base, force)?;
        self.check_generated(files, record, &save_base, force)?;
        let save_work = self.merge_save(&order, &record.mods)?;
        let mut written = None;
        if !sources.is_empty() {
            // build first: if it fails, the installed container stays as it was
            note!("== merging the table edits of {}", order.join(", "));
            let result = edits::build(&self.game_for_edits()?, &sources)?;
            for warning in &result.warnings {
                // "<table>.<row>: ..."
                let editors = warning
                    .split_once(':')
                    .and_then(|(place, _)| place.split_once('.'))
                    .and_then(|(table, row)| {
                        row_editors.get(&(table.to_string(), row.to_string()))
                    });
                report::warning_about(editors.map(Vec::as_slice).unwrap_or(&[]), warning.clone());
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
        if self.replace_generated(transaction, record, &save_base, save_work.as_deref())?
            && save_work.is_none()
        {
            note!("no installed mod edits the template save any more: removed the merged one");
        }
        if let Some(work) = save_work {
            let _ = std::fs::remove_dir_all(&work);
        }
        let removed = self.replace_generated(transaction, record, &base, written.as_deref())?;
        match written {
            Some(work) => drop(std::fs::remove_dir_all(&work)),
            None if removed => {
                note!("no installed mod edits tables any more: removed the merged data container")
            }
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
        note!(
            "   re-merging the installed mods ({}) against the new tables",
            names.join(", ")
        );
        let files = GameFiles::new(&self.game.root)?;
        let mut transaction = files.begin()?;
        self.merge(&mut record, false, &mut transaction)?;
        self.save_record(&mut transaction, &record)?;
        transaction.commit()
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
                    note!(
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
        let problems =
            Self::files_not_free(files, name, new_files, record, manifests, &HashSet::new())?;
        if !problems.is_empty() {
            bail!(
                "can't install {name}: its files would replace files that aren't its own:\n   {}",
                problems.join("\n   ")
            );
        }
        Ok(())
    }

    /// The files a mod would write that aren't free for it (see [`Self::check_files_free`]),
    /// leaving out `freed`: files that are deleted first, in the same transaction.
    fn files_not_free(
        files: &GameFiles,
        name: &str,
        new_files: &[FileRecord],
        record: &Record,
        manifests: &[(String, Manifest)],
        freed: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let previous = manifests
            .iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, manifest)| manifest);
        let mut problems = vec![];
        for file in new_files.iter().filter(|file| !freed.contains(&file.path)) {
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
        Ok(problems)
    }

    fn write_manifest(&self, transaction: &mut Transaction, manifest: &Manifest) -> Result<()> {
        transaction.write(
            &GameFiles::manifest_path(&manifest.name),
            (serde_json::to_string_pretty(manifest)? + "\n").as_bytes(),
        )
    }

    pub(crate) fn manifest_for(
        info: &Entry,
        installed_by: &str,
        files: Vec<FileRecord>,
    ) -> Manifest {
        Manifest {
            name: info.name.clone(),
            title: info.title.clone(),
            version: info.version.clone(),
            game_build: info.game_build.clone(),
            dependencies: info.dependencies.clone(),
            installed_by: installed_by.to_string(),
            tables: info.tables.clone(),
            new_buildings: info.new_buildings.clone(),
            detours: info.detours.clone(),
            hooks: info.hooks.clone(),
            cautions: info.cautions.clone(),
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
        if installing.is_empty() {
            check_game_closed()?;
        }
        if with_dependencies {
            for dependency in mod_
                .requires
                .iter()
                .filter(|dependency| dependency.kind.is_required())
            {
                let current = self.record()?.mods.get(&dependency.name).cloned();
                // the tool carries its own Lua library, for mods that depend on it: written
                // when missing, and updated when the copy is older than the tool's
                if dependency.name == "AlchemistsLibrary" {
                    crate::probe::write_library(&self.library_home())?;
                }
                let local = self.local_folder(&dependency.name);
                let newer = match (&current, &local) {
                    (Some(current), Some(local)) => Mod::load(local)
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
                    let Some(local) = local else {
                        bail!(
                            "{} needs {}, which isn't in the mods folder",
                            mod_.name,
                            requirement_text(dependency)
                        );
                    };
                    let mut chain = installing.to_vec();
                    chain.push(mod_.name.clone());
                    self.install(&Mod::load(&local)?, true, force, &chain)?;
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

        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        record.mods.insert(info.name.clone(), info.clone());
        self.merge(&mut record, force, &mut transaction)?;
        for file in &new_files {
            transaction.copy(&build_folder.join(&file.path), &file.path)?;
        }
        let gone = transaction.delete_listed(&stale, false)?;
        for kept in &gone.kept {
            note!(
                "   kept {kept}: no longer part of {}, but changed since it was installed",
                info.name
            );
        }
        self.write_manifest(
            &mut transaction,
            &Self::manifest_for(&info, "alchemists-tools", new_files),
        )?;
        self.save_record(&mut transaction, &record)?;
        transaction.commit()?;
        note!(
            "installed {} {} (load order: {}); restart the game to load it",
            info.name,
            info.version,
            record.load_order.join(", ")
        );
        if !info.new_buildings.is_empty() {
            note!(
                "   before uninstalling, deconstruct every placed {} in every save you keep: a \
                save with \
                 one placed crashes the game without the mod",
                info.new_buildings.join(", ")
            );
        }
        let unknown = self.unowned_containers(&files, &record)?;
        if !unknown.is_empty() {
            warning!(
                "~mods has containers no manifest lists ({}); if one replaces a table \
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
        check_game_closed()?;
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
                note!("{name} is not installed");
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
            let mut transaction = files.begin()?;
            for path in transaction.delete_unlisted(&named_files)? {
                note!("removed {path}");
            }
            transaction.commit()?;
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
        // a mod no folder of mods holds is kept as a zip there first, to install it again
        if self.local_folder(name).is_none() {
            let kept_entry = entry.clone().or_else(|| {
                manifest
                    .as_ref()
                    .map(|found| entry_from_manifest(name, found))
            });
            if let Some(kept_entry) = kept_entry {
                self.keep_copy(&files, &kept_entry, manifest.as_ref(), force)?;
            }
        }
        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        if entry.is_some() {
            record.mods.shift_remove(name);
            self.merge(&mut record, force, &mut transaction)?;
        }
        let mut kept = vec![];
        if let Some(manifest) = &manifest {
            let result = transaction.delete_listed(&manifest.files, force)?;
            for path in &result.deleted {
                note!("removed {path}");
            }
            for path in &result.missing {
                note!("already gone: {path}");
            }
            kept = result.kept;
            transaction.delete(&GameFiles::manifest_path(name))?;
        } else {
            for path in transaction.delete_unlisted(&unverified)? {
                note!("removed {path}");
            }
        }
        self.save_record(&mut transaction, &record)?;
        transaction.commit()?;
        note!("uninstalled {name}");
        if !kept.is_empty() {
            note!(
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
            note!(
                "reminder: saves with a placed {} will crash without the mod; reinstall it to \
                deconstruct them",
                buildings.join(", ")
            );
        }
        Ok(())
    }

    /// The merged files of a record from before the tool kept hashes (mods.py): they're the
    /// tool's, and the next install or uninstall records them.
    fn legacy_merged(&self, record: &Record) -> Vec<String> {
        if !record.generated.is_empty() || record.mods.is_empty() {
            return vec![];
        }
        let prefix = PAK_FOLDER.join("/");
        [data_container(MERGED), save_container(MERGED_SAVE)]
            .iter()
            .flat_map(|base| {
                ["pak", "ucas", "utoc"].map(|extension| format!("{prefix}/{base}.{extension}"))
            })
            .filter(|path| self.game.root.join(path).is_file())
            .collect()
    }

    /// Who each file in the game folder that a mod or the tool accounts for belongs to: path ->
    /// the mod's name (`<name> (recorded)` for a mod without a manifest), or `the tool`.
    fn owners(&self, record: &Record, manifests: &[(String, Manifest)]) -> HashMap<String, String> {
        let mut owned = HashMap::new();
        for (name, manifest) in manifests {
            for file in &manifest.files {
                owned.insert(file.path.clone(), name.clone());
            }
            owned.insert(GameFiles::manifest_path(name), name.clone());
        }
        for entry in record.mods.values() {
            if !manifests.iter().any(|(owner, _)| *owner == entry.name) {
                for file in &entry.files {
                    owned.insert(file.clone(), format!("{} (recorded)", entry.name));
                }
            }
        }
        for path in self
            .legacy_merged(record)
            .into_iter()
            .chain(record.generated.keys().cloned())
            .chain([format!("{}/{RECORD}", PAK_FOLDER.join("/"))])
        {
            owned.insert(path, "the tool".into());
        }
        owned
    }

    /// Everything else that could be a mod: the files below Paks (not the game's own) that no mod
    /// accounts for, and UE4SS mod folders (not those UE4SS comes with: `Mods/<x>`, or
    /// `Mods/shared/<x>`) -> the files in them no mod accounts for.
    fn unaccounted(
        &self,
        owned: &HashMap<String, String>,
    ) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
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
        (unknown, unknown_ue4ss)
    }

    /// What every mod in the game folder is, and whether its files are as installed: mods with
    /// manifests, mods recorded without one, the tool's merged files, and everything no mod
    /// accounts for (with what its containers replace), plus containers that replace the same
    /// asset. Mods installed from a zip are added to the install record.
    pub fn scan(&self) -> Result<()> {
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        let manifests = files.manifests()?;
        let owned = self.owners(&record, &manifests); // path -> who it belongs to

        note!("== mods with manifests");
        if manifests.is_empty() {
            note!("   none");
        }
        let mut added = vec![];
        for (name, manifest) in &manifests {
            let (mut unchanged, mut changed, mut missing) = (0, vec![], vec![]);
            for file in &manifest.files {
                match files.state(file)? {
                    State::Unchanged => unchanged += 1,
                    State::Changed => changed.push(file.path.clone()),
                    State::Missing => missing.push(file.path.clone()),
                }
            }
            note!(
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
                record
                    .mods
                    .insert(name.clone(), entry_from_manifest(name, manifest));
                added.push(name.clone());
            }
        }
        // saved now, before the rest of the report (whose output may be cut short)
        if !added.is_empty() {
            record.load_order = load_order(&record.mods)?;
            write_json(&self.record_path(), &record)?;
        }

        let unmanifested: Vec<&Entry> = record
            .mods
            .values()
            .filter(|entry| !manifests.iter().any(|(owner, _)| *owner == entry.name))
            .collect();
        if !unmanifested.is_empty() {
            note!("\n== recorded without a manifest (installed by mods.py or an older version)");
            for entry in unmanifested {
                let present = entry
                    .files
                    .iter()
                    .filter(|file| self.game.root.join(file).is_file())
                    .count();
                note!(
                    "   {} {}: {present} of {} recorded file(s) present; reinstall it with this \
                    tool to give it a manifest",
                    entry.name,
                    entry.version,
                    entry.files.len()
                );
            }
        }

        note!("\n== files the tool merges for all installed mods");
        let legacy = self.legacy_merged(&record);
        if record.generated.is_empty() && legacy.is_empty() {
            note!("   none recorded");
        }
        for path in legacy {
            note!("   {path}: written without a hash (by mods.py); the next install records it");
        }
        for (path, file) in &record.generated {
            let state = match files.state(file)? {
                State::Unchanged => "as written",
                State::Changed => "CHANGED since the tool wrote it",
                State::Missing => "MISSING",
            };
            note!("   {path}: {state}");
        }

        let (unknown, unknown_ue4ss) = self.unaccounted(&owned);
        let paks = join(&self.game.root, &["AlchemyFactory", "Content", "Paks"]);
        let ue4ss_prefix = UE4SS_FOLDER.join("/");
        let folder_owner = |top: &str| folder_owner(&owned, top);
        note!("\n== not from any mod this tool knows");
        if unknown.is_empty() && unknown_ue4ss.is_empty() {
            note!("   nothing");
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
            note!(
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
                Some(owner) => note!(
                    "   {ue4ss_prefix}/{top}/: {} file(s) {owner}'s manifest doesn't list ({})",
                    found.len(),
                    found.join(", ")
                ),
                None => note!(
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
            note!(
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
        note!("\n== assets in more than one mod container (only one copy takes effect)");
        if clashes.is_empty() {
            note!("   none");
        }
        for (asset, containers) in clashes {
            note!("   {asset}: {}", containers.join(", "));
        }

        if !added.is_empty() {
            note!(
                "\nadded to {RECORD} (installed from their zips): {}",
                added.join(", ")
            );
        }
        Ok(())
    }

    pub fn new_mod(&self, name: &str) -> Result<()> {
        if !is_name(name) || name == MERGED || name == MERGED_SAVE {
            bail!("a mod name is letters and digits, starting with a letter");
        }
        if let Some(existing) = self.local_folder(name) {
            bail!("{} already exists", display_path(&existing));
        }
        let folder = self.first_mods().join(name);
        if folder.exists() {
            bail!("{} already exists", display_path(&folder));
        }
        std::fs::create_dir_all(folder.join("edits"))?;
        let mut fields = json!({"name": name, "title": name, "version": "0.1.0", "description": "",
                                "author": "", "homepage": "", "team": "", "dependencies": []});
        // made for the game as it is now
        if let Some(build) = game_build(&self.game) {
            fields["game-build"] = json!(build);
        }
        fields["content"] = json!([]);
        fields["craft-types"] = json!({});
        fields["new-craft-types"] = json!([]);
        write_json(&folder.join("mod.json"), &fields)?;
        // every table in it starts with `_`, so it changes nothing until one is renamed
        std::fs::write(folder.join("edits").join("example.json"), EXAMPLE_EDITS)?;
        note!(
            "made {}: put table edits in edits/ (edits/example.json shows how), a UE4SS mod in \
             ue4ss/, and list cooked content (in /Game/Mods/{name}/ in the stub project) in \
             mod.json",
            display_path(&folder)
        );
        Ok(())
    }

    // --- applying a set of mods ---

    /// Make the installed mods exactly `wanted`, all at once or not at all:
    /// - every wanted mod in the folders of mods is built and installed (again, if it's installed);
    /// - a wanted mod installed from elsewhere (its zip) stays as it is;
    /// - every other installed mod is uninstalled.
    ///
    /// Everything is built and checked before the game folder is touched: the mods' dependencies
    /// and incompatibilities as a set, the files each writes, and the merge (rows two mods add,
    /// functions two mods detour). Then the game folder is changed in one transaction, undone if any
    /// step fails. `force` is as for `install` and `uninstall`: delete files changed since they were
    /// installed and those of mods without a manifest, and replace changed merged files.
    pub fn apply(&self, wanted: &[String], force: bool) -> Result<Applied> {
        check_game_closed()?;
        let files = GameFiles::new(&self.game.root)?;
        if files.recover()? {
            warning!("a change to the game folder was interrupted last time; it has been undone");
        }
        let manifests = files.manifests()?;
        let mut record = self.record()?;
        // mods installed from their zips, which the record may not list yet
        for (name, manifest) in &manifests {
            if !record.mods.contains_key(name) {
                record
                    .mods
                    .insert(name.clone(), entry_from_manifest(name, manifest));
            }
        }
        let wanted: BTreeSet<&String> = wanted.iter().collect();
        let mut problems: Vec<(Vec<String>, String)> = vec![];
        let archives = self.archives();
        let mut from_archives: Vec<&Archive> = vec![];
        let mut to_build: IndexMap<String, Mod> = IndexMap::new();
        for name in &wanted {
            match self.local_folder(name) {
                Some(folder) => match Mod::load(&folder) {
                    Ok(found) if found.name != **name => problems.push((
                        vec![name.to_string()],
                        format!(
                            "{}: its mod.json names it {}; name its folder the same",
                            display_path(&folder),
                            found.name
                        ),
                    )),
                    Ok(found) => {
                        to_build.insert(found.name.clone(), found);
                    }
                    Err(error) => problems.push((vec![name.to_string()], format!("{error:#}"))),
                },
                None if record.mods.contains_key(*name) => {}
                None => match archives.get(*name) {
                    Some(archive) => from_archives.push(archive),
                    None => problems.push((
                        vec![name.to_string()],
                        format!(
                            "there's no mod {name} in the mods folders, and it isn't installed"
                        ),
                    )),
                },
            }
        }
        let removing: Vec<String> = record
            .mods
            .keys()
            .filter(|name| !wanted.contains(name))
            .cloned()
            .collect();
        for name in &removing {
            let has_manifest = manifests.iter().any(|(owner, _)| owner == name);
            let present = record.mods[name]
                .files
                .iter()
                .any(|file| self.game.root.join(file).is_file());
            if !has_manifest && present && !force {
                problems.push((
                    vec![name.clone()],
                    format!(
                        "{name} has no manifest (it was installed by mods.py or an older version), \
                         so nothing confirms its files are still its own: reinstall it first, or \
                         force deleting them"
                    ),
                ));
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't apply", problems));
        }

        // built in load order, so each mod's dependencies (and their cooked assets) are built first
        let planned: IndexMap<String, Entry> = to_build
            .values()
            .map(|found| {
                let entry = Entry {
                    name: found.name.clone(),
                    version: found.version.clone(),
                    dependencies: found.dependencies.clone(),
                    ..Entry::default()
                };
                (found.name.clone(), entry)
            })
            .collect();
        let mut built: Vec<(PathBuf, Entry)> = vec![];
        for name in load_order(&planned)? {
            note!("\n=== {name}");
            match self.build_mod(&to_build[&name]) {
                Ok(result) => built.push(result),
                Err(error) => problems.push((vec![name.clone()], format!("{error:#}"))),
            }
        }
        if !problems.is_empty() {
            return Err(fail("couldn't build every mod", problems));
        }

        // the mods as they'll be, checked as a set
        let mut mods = record.mods.clone();
        for name in &removing {
            mods.shift_remove(name);
        }
        for (_, info) in &built {
            mods.insert(info.name.clone(), info.clone());
        }
        for archive in &from_archives {
            mods.insert(archive.manifest.name.clone(), archive.entry());
        }
        problems.extend(check_together(&mods)?);
        let ue4ss_prefix = format!("{}/", UE4SS_FOLDER.join("/"));
        let needing: Vec<String> = built
            .iter()
            .filter(|(_, info)| info.needs_ue4ss)
            .map(|(_, info)| info.name.clone())
            .chain(
                from_archives
                    .iter()
                    .filter(|archive| {
                        archive
                            .manifest
                            .files
                            .iter()
                            .any(|file| file.path.starts_with(&ue4ss_prefix))
                    })
                    .map(|archive| archive.manifest.name.clone()),
            )
            .collect();
        if !needing.is_empty() && !join(&self.game.root, UE4SS_FOLDER).is_dir() {
            let names: Vec<&String> = needing.iter().collect();
            problems.push((
                needing.clone(),
                format!(
                    "{} need(s) UE4SS, and it isn't installed in {}: install it (Setup in the \
                     GUI, or alchemists-tools ue4ss install)",
                    listed(&names),
                    display_path(&self.game.root)
                ),
            ));
        }
        // each file is free: the uninstalled mods' unchanged files are deleted first
        let mut freed = HashSet::new();
        for (_, manifest) in manifests
            .iter()
            .filter(|(owner, _)| removing.contains(owner))
        {
            for file in &manifest.files {
                if force || files.state(file)? == State::Unchanged {
                    freed.insert(file.path.clone());
                }
            }
        }
        let staying: Vec<(String, Manifest)> = manifests
            .iter()
            .filter(|(owner, _)| !removing.contains(owner))
            .cloned()
            .collect();
        let mut claimed: HashMap<String, String> = HashMap::new();
        let mut new_files = vec![];
        for (folder, info) in &built {
            let records = Self::file_records(folder, &info.files)?;
            for file in &records {
                if let Some(other) = claimed.insert(file.path.clone(), info.name.clone()) {
                    problems.push((
                        vec![other.clone(), info.name.clone()],
                        format!("{other} and {} both install {}", info.name, file.path),
                    ));
                }
            }
            let taken =
                Self::files_not_free(&files, &info.name, &records, &record, &staying, &freed)?;
            for problem in taken {
                problems.push((
                    vec![info.name.clone()],
                    format!(
                        "{}: its files would replace one that isn't its own: {problem}",
                        info.name
                    ),
                ));
            }
            new_files.push(records);
        }
        for archive in &from_archives {
            let name = &archive.manifest.name;
            for file in &archive.install_manifest().files {
                if let Some(other) = claimed.insert(file.path.clone(), name.clone()) {
                    problems.push((
                        vec![other.clone(), name.clone()],
                        format!("{other} and {name} both install {}", file.path),
                    ));
                }
            }
            let taken = Self::files_not_free(
                &files,
                name,
                &archive.install_manifest().files,
                &record,
                &staying,
                &freed,
            )?;
            for problem in taken {
                problems.push((
                    vec![name.clone()],
                    format!("{name}: its files would replace one that isn't its own: {problem}"),
                ));
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't install these mods together", problems));
        }
        // a mod no folder of mods holds is kept as a zip there before it's uninstalled
        for name in &removing {
            if self.local_folder(name).is_none() {
                let manifest = manifests
                    .iter()
                    .find(|(owner, _)| owner == name)
                    .map(|(_, manifest)| manifest);
                if let Err(error) = self.keep_copy(&files, &record.mods[name], manifest, force) {
                    problems.push((vec![name.clone()], format!("{error:#}")));
                }
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't uninstall every mod", problems));
        }

        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        let mut buildings = vec![];
        for name in &removing {
            let entry = &record.mods[name];
            buildings.extend(entry.new_buildings.iter().cloned());
            match manifests.iter().find(|(owner, _)| owner == name) {
                Some((_, manifest)) => {
                    let result = transaction.delete_listed(&manifest.files, force)?;
                    for path in &result.kept {
                        note!("   kept {path}: it has changed since {name} installed it");
                    }
                    transaction.delete(&GameFiles::manifest_path(name))?;
                }
                None => {
                    transaction.delete_unlisted(&entry.files)?;
                }
            }
            note!("uninstalling {name}");
        }
        record.mods = mods;
        self.merge(&mut record, force, &mut transaction)?;
        for ((folder, info), records) in built.iter().zip(new_files) {
            // its old files that the new build doesn't have
            let stale: Vec<FileRecord> = manifests
                .iter()
                .find(|(owner, _)| *owner == info.name)
                .map(|(_, manifest)| {
                    manifest
                        .files
                        .iter()
                        .filter(|old| !records.iter().any(|new| new.path == old.path))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for file in &records {
                transaction.copy(&folder.join(&file.path), &file.path)?;
            }
            for kept in transaction.delete_listed(&stale, false)?.kept {
                note!(
                    "   kept {kept}: no longer part of {}, but changed since it was installed",
                    info.name
                );
            }
            self.write_manifest(
                &mut transaction,
                &Self::manifest_for(info, "alchemists-tools", records),
            )?;
        }
        for archive in &from_archives {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive.path)?)?;
            for file in &archive.install_manifest().files {
                let mut contents = vec![];
                std::io::Read::read_to_end(
                    &mut zip.by_name(&file.path).with_context(|| {
                        format!("{} has no {}", display_path(&archive.path), file.path)
                    })?,
                    &mut contents,
                )?;
                transaction.write(&file.path, &contents)?;
                if transaction.files().record(&file.path)? != *file {
                    bail!(
                        "{}: {} isn't as its manifest says (is the zip damaged?)",
                        display_path(&archive.path),
                        file.path
                    );
                }
            }
            self.write_manifest(&mut transaction, &archive.install_manifest())?;
            note!(
                "installing {} from {}",
                archive.manifest.name,
                display_path(&archive.path)
            );
        }
        self.save_record(&mut transaction, &record)?;
        transaction.commit()?;

        let installed: Vec<String> = built
            .iter()
            .map(|(_, info)| info.name.clone())
            .chain(
                from_archives
                    .iter()
                    .map(|archive| archive.manifest.name.clone()),
            )
            .collect();
        note!(
            "\ninstalled {}; uninstalled {} (load order: {}); restart the game to load them",
            if installed.is_empty() {
                "none".to_string()
            } else {
                installed.join(", ")
            },
            if removing.is_empty() {
                "none".to_string()
            } else {
                removing.join(", ")
            },
            if record.load_order.is_empty() {
                "none".to_string()
            } else {
                record.load_order.join(", ")
            }
        );
        if !buildings.is_empty() {
            note!(
                "reminder: saves with a placed {} will crash without the mod that adds it; \
                 reinstall it to deconstruct them",
                buildings.join(", ")
            );
        }
        let unknown = self.unowned_containers(&files, &record)?;
        if !unknown.is_empty() {
            warning!(
                "~mods has containers no manifest lists ({}); if one replaces a table that \
                 installed mods edit, only one takes effect (alchemists-tools mod scan shows what \
                 they change)",
                unknown.join(", ")
            );
        }
        Ok(Applied {
            installed,
            uninstalled: removing,
            load_order: record.load_order,
        })
    }

    // --- what's there, for listing ---

    /// The mods in the folders of mods and in the game folder, what's installed, and the files no
    /// mod accounts for. Without a game folder, only the mods folders' mods.
    pub fn overview(&self) -> Result<Overview> {
        let files = GameFiles::new(&self.game.root).ok();
        let manifests = match &files {
            Some(files) => files.manifests()?,
            None => vec![],
        };
        let record = self.record()?;
        let mut mods: BTreeMap<String, Summary> = BTreeMap::new();
        for folder in self.mod_folders() {
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            let mut summary = match Mod::load(&folder) {
                Ok(found) => Summary {
                    title: found.title.clone(),
                    version: found.version.clone(),
                    description: found.description.clone(),
                    author: found.author.clone(),
                    contact: found.contact.clone(),
                    homepage: found.homepage.clone(),
                    dependencies: found.dependencies.clone(),
                    thumbnail: found.thumbnail(),
                    game_build: found.game_build.clone(),
                    detours: found.detours.clone().unwrap_or_default(),
                    error: (found.name != name).then(|| {
                        format!(
                            "its mod.json names it {}; name its folder the same",
                            found.name
                        )
                    }),
                    ..Summary::new(&name)
                },
                Err(error) => Summary {
                    error: Some(format!("{error:#}")),
                    ..Summary::new(&name)
                },
            };
            summary.ignored_copies = self
                .every_mod_folder()
                .into_iter()
                .filter(|copy| {
                    *copy != folder && copy.file_name().is_some_and(|other| other == name.as_str())
                })
                .collect();
            summary.git = self.git_checkout(&folder).map(|(_, url)| url);
            summary.folder = Some(folder);
            mods.insert(name, summary);
        }
        for (name, archive) in self.archives() {
            let manifest = &archive.manifest;
            let summary = mods.entry(name.clone()).or_insert_with(|| Summary {
                title: manifest.title.clone(),
                version: manifest.version.clone(),
                dependencies: manifest.dependencies.clone(),
                game_build: manifest.game_build.clone(),
                detours: manifest.detours.clone(),
                hooks: manifest.hooks.clone(),
                tables: manifest.tables.clone(),
                new_buildings: manifest.new_buildings.clone(),
                cautions: manifest.cautions.clone(),
                ..Summary::new(&name)
            });
            if summary.folder.is_some() {
                summary.ignored_copies.push(archive.path);
            } else {
                summary.archive = Some(archive.path);
            }
        }
        for (name, manifest) in &manifests {
            let summary = mods.entry(name.clone()).or_insert_with(|| Summary {
                title: manifest.title.clone(),
                version: manifest.version.clone(),
                dependencies: manifest.dependencies.clone(),
                detours: manifest.detours.clone(),
                ..Summary::new(name)
            });
            let (mut changed, mut missing) = (vec![], vec![]);
            if let Some(files) = &files {
                for file in &manifest.files {
                    match files.state(file)? {
                        State::Unchanged => {}
                        State::Changed => changed.push(file.path.clone()),
                        State::Missing => missing.push(file.path.clone()),
                    }
                }
            }
            summary.installed = Some(Installed {
                version: manifest.version.clone(),
                game_build: manifest.game_build.clone(),
                origin: match manifest.installed_by.as_str() {
                    "zip" => Origin::Zip,
                    "import" => Origin::Imported,
                    _ => Origin::Tool,
                },
                changed,
                missing,
            });
            summary.hooks = manifest.hooks.clone();
            summary.cautions = manifest.cautions.clone();
            summary.tables = manifest.tables.clone();
            summary.new_buildings = manifest.new_buildings.clone();
        }
        for (name, entry) in &record.mods {
            let summary = mods.entry(name.clone()).or_insert_with(|| Summary {
                title: entry.title.clone(),
                version: entry.version.clone(),
                dependencies: entry.dependencies.clone(),
                detours: entry.detours.clone(),
                ..Summary::new(name)
            });
            if summary.installed.is_none() {
                summary.installed = Some(Installed {
                    version: entry.version.clone(),
                    game_build: entry.game_build.clone(),
                    origin: Origin::Unverified,
                    changed: vec![],
                    missing: entry
                        .files
                        .iter()
                        .filter(|file| !self.game.root.join(file).is_file())
                        .cloned()
                        .collect(),
                });
                summary.tables = entry.tables.clone();
                summary.new_buildings = entry.new_buildings.clone();
            }
            if summary.hooks.is_empty() {
                summary.hooks = entry.hooks.clone();
            }
        }
        // a mod's game build: its mod.json's, or else the one it was installed against
        for summary in mods.values_mut() {
            if summary.game_build.is_none() {
                summary.game_build = summary
                    .installed
                    .as_ref()
                    .and_then(|installed| installed.game_build.clone());
            }
        }
        let unmanaged = if files.is_some() {
            self.unmanaged(&record, &manifests)
        } else {
            vec![]
        };
        Ok(Overview {
            mods: mods.into_values().collect(),
            unmanaged,
            game_build: game_build(&self.game),
            load_order: record.load_order,
        })
    }

    /// The files in the game folder no mod accounts for, grouped by the mod they look like.
    fn unmanaged(&self, record: &Record, manifests: &[(String, Manifest)]) -> Vec<Unmanaged> {
        let owned = self.owners(record, manifests);
        let (unknown, unknown_ue4ss) = self.unaccounted(&owned);
        let mut found: BTreeMap<String, Unmanaged> = BTreeMap::new();
        let game_packages = if unknown.is_empty() {
            HashSet::new()
        } else {
            crate::unpack::package_names(&self.game.paks().join("AlchemyFactory-Windows.utoc"))
                .unwrap_or_default()
        };
        for path in &unknown {
            // `~mods/zzz_Name_P.pak` -> Name
            let file = path.rsplit('/').next().unwrap_or(path);
            let (stem, extension) = file.rsplit_once('.').unwrap_or((file, ""));
            let name = stem.strip_prefix("zzz_").unwrap_or(stem);
            let name = name.strip_suffix("_P").unwrap_or(name).to_string();
            let entry = found
                .entry(name.clone())
                .or_insert_with(|| Unmanaged::new(&name));
            entry.files.push(path.clone());
            if extension == "utoc" || extension == "pak" {
                let mut replaces: Vec<String> = container_contents(&self.game.root.join(path))
                    .into_iter()
                    .filter(|package| game_packages.contains(package))
                    .collect();
                entry.replaces.append(&mut replaces);
            }
        }
        let prefix = UE4SS_FOLDER.join("/");
        for (top, inner) in &unknown_ue4ss {
            // files a known mod's manifest doesn't list, in its folder, aren't another mod
            if folder_owner(&owned, top).is_some() {
                continue;
            }
            let name = top.strip_prefix("shared/").unwrap_or(top).to_string();
            let entry = found
                .entry(name.clone())
                .or_insert_with(|| Unmanaged::new(&name));
            entry
                .files
                .extend(inner.iter().map(|file| format!("{prefix}/{top}/{file}")));
        }
        // AF Mod Loader's mods, in <game>/mods/
        for entry in std::fs::read_dir(self.game.root.join("mods"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let files: Vec<String> = if entry.path().is_dir() {
                files_below(&entry.path())
                    .into_iter()
                    .map(|file| format!("mods/{name}/{file}"))
                    .collect()
            } else {
                vec![format!("mods/{name}")]
            };
            let key = format!("{name} (AF Mod Loader)");
            found.insert(
                key,
                Unmanaged {
                    mod_loader: true,
                    files,
                    ..Unmanaged::new(&name)
                },
            );
        }
        for entry in found.values_mut() {
            entry.replaces.sort();
            entry.replaces.dedup();
        }
        found.into_values().collect()
    }

    /// The mods in the mods folders, what's installed, and the load order.
    pub fn list(&self) -> Result<()> {
        let overview = self.overview()?;
        let current = overview.game_build.as_deref();
        for summary in &overview.mods {
            let state = match (&summary.installed, &summary.folder) {
                (Some(installed), Some(_)) if installed.version != summary.version => {
                    format!("installed ({})", installed.version)
                }
                (Some(_), Some(_)) => "installed".to_string(),
                (Some(_), None) => "installed (not in the mods folders)".to_string(),
                (None, _) => "not installed".to_string(),
            };
            let notes: String = summary
                .notes(current)
                .iter()
                .map(|note| format!("\n   {note}"))
                .collect();
            note!("{} {}: {state}{notes}", summary.name, summary.version);
        }
        note!(
            "{} of {} mod(s) installed",
            overview.installed().count(),
            overview.mods.len()
        );
        if !overview.unmanaged.is_empty() {
            note!("not installed by this tool (mod scan shows more):");
            for unmanaged in &overview.unmanaged {
                note!("   {}: {}", unmanaged.name, unmanaged.describe());
            }
        }
        if !overview.load_order.is_empty() {
            note!("load order: {}", overview.load_order.join(", "));
        }
        Ok(())
    }
}

/// Every mod folder (one with a mod.json) in `folders` (folders of mods), in the order they're
/// taken: each folder's own mod folders first, then those in the git checkouts it holds
/// (`<checkout>/<mod>/` or `<checkout>/mods/<mod>/`). A name can come more than once.
pub fn mod_folders_in(folders: &[PathBuf]) -> Vec<PathBuf> {
    let folders_in = |folder: &Path| -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            })
            .collect();
        folders.sort();
        folders
    };
    let (mut direct, mut checkouts) = (vec![], vec![]);
    for mods in folders {
        for folder in folders_in(mods) {
            if folder.join("mod.json").is_file() {
                direct.push(folder);
            } else if folder.join(".git").exists() {
                for inside in [folder.clone(), folder.join("mods")] {
                    checkouts.extend(
                        folders_in(&inside)
                            .into_iter()
                            .filter(|inner| inner.join("mod.json").is_file()),
                    );
                }
            }
        }
    }
    direct.extend(checkouts);
    direct
}

/// Write a mod's zip as this tool reads it from a mods folder (see [`Archive`]): its files at their
/// paths in the game folder, its file manifest and its install record entry. It's written whole
/// beside `path` first, so a failure never leaves half a zip that looks like a mod.
pub(crate) fn write_mod_zip(
    path: &Path,
    manifest: &Manifest,
    entry: &Entry,
    files: Vec<(String, Vec<u8>)>,
) -> Result<()> {
    use std::io::Write;
    let part = path.with_extension("zip.part");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&part)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (relative, contents) in files {
        zip.start_file(relative, options)?;
        zip.write_all(&contents)?;
    }
    zip.start_file(GameFiles::manifest_path(&manifest.name), options)?;
    zip.write_all((serde_json::to_string_pretty(manifest)? + "\n").as_bytes())?;
    zip.start_file(ENTRY_FILE, options)?;
    zip.write_all((serde_json::to_string_pretty(entry)? + "\n").as_bytes())?;
    zip.finish()?;
    std::fs::rename(&part, path).with_context(|| format!("writing {}", display_path(path)))?;
    Ok(())
}

/// In a mod's zip (one `mod dist`, `mod import` or [`Workspace::keep_copy`] made): the mod's
/// install record entry, with the edits that are merged for it.
pub(crate) const ENTRY_FILE: &str = "alchemists-tools-entry.json";

/// A mod as a zip in a folder of mods: one `mod dist` made (a download), one `mod import` made, or
/// one this tool kept when uninstalling a mod it had no folder for. It holds the mod's files as
/// they're installed and its file manifest, and, unless it's from an older version of the tool,
/// its install record entry.
#[derive(Clone, Debug)]
pub struct Archive {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub entry: Option<Entry>,
}

impl Archive {
    /// The manifest of the mod as this tool installs it: with an install record entry, its edits
    /// are merged, so its own data container and template save (for installing by hand) are left
    /// out.
    pub fn install_manifest(&self) -> Manifest {
        let mut manifest = self.manifest.clone();
        if let Some(entry) = &self.entry {
            manifest
                .files
                .retain(|file| !entry.data_files.contains(&file.path));
            if manifest.installed_by == "zip" {
                manifest.installed_by = "alchemists-tools".to_string();
            }
        }
        manifest
    }

    /// Its install record entry: as in the zip, or else from its manifest (a zip from an older
    /// version of the tool, whose table edits are in its own container).
    fn entry(&self) -> Entry {
        let manifest = self.install_manifest();
        let mut entry = self
            .entry
            .clone()
            .unwrap_or_else(|| entry_from_manifest(&manifest.name, &manifest));
        entry.files = manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        entry
    }
}

/// The `manifest.json` at the top of a mod's zip (Thunderstore's, see [`crate::sharing`]), if it
/// has one.
pub(crate) fn read_root_manifest(path: &Path) -> Option<Value> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let file = zip.by_name("manifest.json").ok()?;
    serde_json::from_reader(file).ok()
}

/// A zip holding one mod's manifest (`manifests/<name>.json`), read; None for any other zip.
fn read_archive(path: &Path) -> Option<Archive> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let prefix = format!("{}/", crate::files::MANIFESTS);
    let manifests: Vec<String> = zip
        .file_names()
        .filter(|name| name.starts_with(&prefix) && name.ends_with(".json"))
        .map(str::to_string)
        .collect();
    let [manifest_name] = manifests.as_slice() else {
        return None;
    };
    let manifest: Manifest = serde_json::from_reader(zip.by_name(manifest_name).ok()?).ok()?;
    let entry = zip
        .by_name(ENTRY_FILE)
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok());
    Some(Archive {
        path: path.to_path_buf(),
        manifest,
        entry,
    })
}

/// What [`Workspace::apply`] did.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub installed: Vec<String>,
    pub uninstalled: Vec<String>,
    pub load_order: Vec<String>,
}

/// What's wrong with a set of mods installed together, each with the mods it's about: a required
/// mod missing or the wrong version, two incompatible mods, two craft types with one number.
fn check_together(mods: &IndexMap<String, Entry>) -> Result<Vec<(Vec<String>, String)>> {
    let mut problems = vec![];
    for (name, entry) in mods {
        for dependency in entry.requires()? {
            let about = vec![name.clone(), dependency.name.clone()];
            match (dependency.kind, mods.get(&dependency.name)) {
                (Kind::Incompatible, Some(_)) => problems.push((
                    about,
                    format!("{name} doesn't work with {}", dependency.name),
                )),
                (kind, None) if kind.is_required() => problems.push((
                    vec![name.clone()],
                    format!(
                        "{name} needs {}, which isn't among the mods to install",
                        requirement_text(&dependency)
                    ),
                )),
                (Kind::Recommended, None) => {
                    note!("note: {name} recommends {}", requirement_text(&dependency))
                }
                (_, None) => {}
                (_, Some(other)) => {
                    if !satisfies(other.version()?, &dependency) {
                        problems.push((
                            about,
                            format!(
                                "{name} needs {}, but {} is {}",
                                requirement_text(&dependency),
                                dependency.name,
                                other.version
                            ),
                        ));
                    }
                }
            }
        }
    }
    let mut numbers: HashMap<u8, (String, String)> = HashMap::new(); // number -> (key, mod)
    for (name, entry) in mods {
        for (key, number) in &entry.craft_type_numbers {
            match numbers.get(number) {
                Some((first_key, first_mod)) if first_key != key => problems.push((
                    vec![first_mod.clone(), name.clone()],
                    format!(
                        "craft type {number} is both {first_key} and {key} (mods made with \
                         different allocations.json files; one of them needs a new number)"
                    ),
                )),
                Some(_) => {}
                None => {
                    numbers.insert(*number, (key.clone(), name.clone()));
                }
            }
        }
    }
    Ok(problems)
}

/// Report each problem as an error about its mods (for the GUI to mark them), and return one
/// error listing them all.
fn fail(heading: &str, problems: Vec<(Vec<String>, String)>) -> anyhow::Error {
    for (mods, text) in &problems {
        report::send(report::Level::Error, mods, text.clone());
    }
    let texts: Vec<&str> = problems.iter().map(|(_, text)| text.as_str()).collect();
    anyhow::anyhow!("{heading}:\n   {}", texts.join("\n   "))
}

/// How an installed mod was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// By this tool, which wrote its manifest.
    Tool,
    /// From its zip, by hand: its table edits are in its own container, not merged with the other
    /// mods'.
    Zip,
    /// By mods.py or an older version of this tool, without a manifest: nothing confirms its files
    /// are still its own.
    Unverified,
    /// From a mod made without this tool, by `mod import`, as best it could.
    Imported,
}

/// An installed mod, as installed.
#[derive(Clone, Debug)]
pub struct Installed {
    pub version: String,
    /// The game's build when it was built.
    pub game_build: Option<String>,
    pub origin: Origin,
    /// Its files that have changed since it installed them.
    pub changed: Vec<String>,
    pub missing: Vec<String>,
}

/// A mod in the folders of mods, installed, or both.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub contact: String,
    pub homepage: String,
    pub dependencies: Vec<String>,
    /// Its folder, if it's in the folders of mods.
    pub folder: Option<PathBuf>,
    /// Mods of the same name in later folders of mods, which are ignored.
    pub ignored_copies: Vec<PathBuf>,
    /// What's wrong with its folder (its mod.json can't be read, say).
    pub error: Option<String>,
    pub thumbnail: Option<PathBuf>,
    /// The zip it's kept as in a folder of mods, if it has no folder (see [`Workspace::archives`]).
    pub archive: Option<PathBuf>,
    /// The URL of the git repository it was added from (`mod add`).
    pub git: Option<String>,
    /// The game build it's made for (its mod.json's `game-build`), or else the one it was built
    /// against when installed.
    pub game_build: Option<String>,
    pub installed: Option<Installed>,
    /// The game functions its C++ detours, and those it hooks through UE4SS (once installed).
    pub detours: Vec<String>,
    pub hooks: Vec<String>,
    /// The game tables its edits change, and the buildings it adds (once installed).
    pub tables: Vec<String>,
    pub new_buildings: Vec<String>,
    /// What couldn't be checked about it (an imported mod's: see [`crate::import`]).
    pub cautions: Vec<String>,
}

impl Summary {
    fn new(name: &str) -> Self {
        Summary {
            name: name.to_string(),
            title: name.to_string(),
            ..Summary::default()
        }
    }

    pub fn requires(&self) -> Vec<Dependency> {
        self.dependencies
            .iter()
            .filter_map(|text| parse_dependency(text, &self.name).ok())
            .collect()
    }

    /// Whether it can be built and installed: it's in a mods folder, which is as it should be.
    pub fn installable(&self) -> bool {
        (self.folder.is_some() && self.error.is_none()) || self.archive.is_some()
    }

    /// Whether the game is a newer build than the one it's made for.
    pub fn outdated(&self, current: Option<&str>) -> bool {
        match (&self.game_build, current) {
            (Some(made_for), Some(current)) => older_build(made_for, current),
            _ => false,
        }
    }

    /// What needs attention about it: why it can't be installed, how it was installed if that
    /// needs care, files changed since, an older game build.
    pub fn notes(&self, current: Option<&str>) -> Vec<String> {
        let mut notes = vec![];
        if let Some(error) = &self.error {
            notes.push(format!("can't be installed: {error}"));
        }
        if let Some(installed) = &self.installed {
            match installed.origin {
                Origin::Tool => {}
                Origin::Zip => notes.push(
                    "installed from its zip: its table edits are in its own container, not \
                     merged with the other mods' (install it from its mod folder to merge them)"
                        .to_string(),
                ),
                Origin::Unverified => notes.push(
                    "installed without a manifest (by mods.py or an older version), so its files \
                     can't be checked: reinstall it to give it one"
                        .to_string(),
                ),
                Origin::Imported => {}
            }
            if !installed.changed.is_empty() {
                notes.push(format!(
                    "changed since it was installed: {}",
                    installed.changed.join(", ")
                ));
            }
            if !installed.missing.is_empty() {
                notes.push(format!("missing: {}", installed.missing.join(", ")));
            }
        }
        if self.outdated(current) {
            notes.push(format!(
                "made for game build {}, and the game is build {} now: it may need updating",
                self.game_build.as_deref().unwrap_or_default(),
                current.unwrap_or_default()
            ));
        }
        notes.extend(
            self.cautions
                .iter()
                .map(|caution| format!("caution: {caution}")),
        );
        if self.folder.is_none() {
            match (&self.archive, &self.installed) {
                (Some(archive), _) => notes.push(format!(
                    "a zip in the mods folder ({}): it can be installed and uninstalled, not \
                     rebuilt",
                    archive
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )),
                (None, Some(_)) => notes.push(
                    "not in the mods folders: uninstalling it keeps a copy there, as a zip, to \
                     install it again"
                        .to_string(),
                ),
                (None, None) => {}
            }
        }
        if !self.ignored_copies.is_empty() {
            let copies: Vec<String> = self
                .ignored_copies
                .iter()
                .map(|copy| display_path(copy))
                .collect();
            notes.push(format!(
                "also in {}, which {} ignored (the first mods folder's copy is used)",
                copies.join(", "),
                if copies.len() == 1 { "is" } else { "are" }
            ));
        }
        notes
    }
}

/// Files in the game folder that no mod accounts for: installed by hand, or by another tool.
#[derive(Clone, Debug, Default)]
pub struct Unmanaged {
    /// The mod they look like: a container's mod name, or a UE4SS mod's folder.
    pub name: String,
    /// Relative to the game folder.
    pub files: Vec<String>,
    /// Game assets its containers replace.
    pub replaces: Vec<String>,
    /// AF Mod Loader's (in `<game>/mods/`).
    pub mod_loader: bool,
}

impl Unmanaged {
    fn new(name: &str) -> Self {
        Unmanaged {
            name: name.to_string(),
            ..Unmanaged::default()
        }
    }

    /// Where it is and what it does, in a line.
    pub fn describe(&self) -> String {
        let place = if self.mod_loader {
            "an AF Mod Loader mod"
        } else if self.files.iter().any(|file| file.contains("/ue4ss/Mods/")) {
            if self.files.iter().any(|file| file.contains("/Paks/")) {
                "containers and a UE4SS mod"
            } else {
                "a UE4SS mod"
            }
        } else {
            "containers"
        };
        let mut text = format!("{place}, {} file(s)", self.files.len());
        if !self.replaces.is_empty() {
            let shown: Vec<&str> = self.replaces.iter().take(4).map(String::as_str).collect();
            text += &format!(
                "; replaces {}{}",
                shown.join(", "),
                if self.replaces.len() > 4 { ", ..." } else { "" }
            );
        }
        text
    }
}

/// Everything in the mods folders and the game folder (see [`Workspace::overview`]).
#[derive(Clone, Debug, Default)]
pub struct Overview {
    /// By name.
    pub mods: Vec<Summary>,
    pub unmanaged: Vec<Unmanaged>,
    /// The game's Steam build.
    pub game_build: Option<String>,
    pub load_order: Vec<String>,
}

impl Overview {
    pub fn get(&self, name: &str) -> Option<&Summary> {
        self.mods.iter().find(|summary| summary.name == name)
    }

    pub fn installed(&self) -> impl Iterator<Item = &Summary> {
        self.mods
            .iter()
            .filter(|summary| summary.installed.is_some())
    }

    /// Tick `name` in `ticked`, and the mods it requires, directly or not (those listed).
    pub fn tick(&self, ticked: &mut BTreeSet<String>, name: &str) {
        let mut seen = HashSet::new();
        let mut pending = vec![name.to_string()];
        while let Some(current) = pending.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            ticked.insert(current.clone());
            if let Some(summary) = self.get(&current) {
                pending.extend(
                    summary
                        .requires()
                        .into_iter()
                        .filter(|dependency| {
                            dependency.kind.is_required() && self.get(&dependency.name).is_some()
                        })
                        .map(|dependency| dependency.name),
                );
            }
        }
    }

    /// Untick `name` in `ticked`, and the mods that require it, directly or not.
    pub fn untick(&self, ticked: &mut BTreeSet<String>, name: &str) {
        let mut pending = vec![name.to_string()];
        while let Some(current) = pending.pop() {
            if !ticked.remove(&current) && current != name {
                continue;
            }
            pending.extend(
                self.mods
                    .iter()
                    .filter(|summary| {
                        ticked.contains(&summary.name)
                            && summary.requires().iter().any(|dependency| {
                                dependency.name == current && dependency.kind.is_required()
                            })
                    })
                    .map(|summary| summary.name.clone()),
            );
        }
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
    fn hooks_are_found() {
        let script = "RegisterHook(\"/Script/BeltTD.BeltTDInventoryComponent:CheckCanCost\", \
                      function() end)\nRegisterHook(path, function() end)";
        assert_eq!(
            string_arguments(script, "RegisterHook"),
            ["/Script/BeltTD.BeltTDInventoryComponent:CheckCanCost"]
        );
        // C++'s STR("...") is UTF-16, at any offset
        let mut dll = vec![0x90u8; 7];
        for unit in "/Script/BeltTD.BeltTDInventoryHelper:GetInventoryMaximumStack\0".encode_utf16()
        {
            dll.extend(unit.to_le_bytes());
        }
        dll.extend(b"junk /Script/Engine.Actor:ReceiveBeginPlay\0");
        assert_eq!(
            function_paths(&dll),
            [
                "/Script/BeltTD.BeltTDInventoryHelper:GetInventoryMaximumStack",
                "/Script/Engine.Actor:ReceiveBeginPlay"
            ]
        );
    }

    fn entry(name: &str, dependencies: &[&str]) -> Entry {
        Entry {
            name: name.into(),
            version: "0.1.0".into(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
            ..Entry::default()
        }
    }

    #[test]
    fn mods_are_checked_as_a_set() {
        let mods: IndexMap<String, Entry> = [
            entry("A", &["B >= 0.2.0"]),
            entry("B", &[]),
            entry("C", &["! B", "D"]),
        ]
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect();
        let problems = check_together(&mods).unwrap();
        let texts: Vec<&str> = problems.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "A needs B >= 0.2.0, but B is 0.1.0",
                "C doesn't work with B",
                "C needs D, which isn't among the mods to install"
            ]
        );
        assert_eq!(problems[1].0, ["C", "B"]);
    }

    #[test]
    fn ticking_follows_dependencies() {
        let summary = |name: &str, dependencies: &[&str]| Summary {
            dependencies: dependencies.iter().map(|text| text.to_string()).collect(),
            ..Summary::new(name)
        };
        let overview = Overview {
            mods: vec![
                summary("Library", &[]),
                summary("Vault", &["Library >= 0.1.0", "? Other"]),
                summary("Addon", &["Vault"]),
                summary("Other", &[]),
            ],
            ..Overview::default()
        };
        let mut ticked = BTreeSet::new();
        overview.tick(&mut ticked, "Addon");
        assert_eq!(
            ticked.iter().collect::<Vec<_>>(),
            ["Addon", "Library", "Vault"]
        );
        overview.untick(&mut ticked, "Library");
        assert!(ticked.is_empty());
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
