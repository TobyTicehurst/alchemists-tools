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

mod apply;
mod build;
mod install;
mod listing;
mod manifest;
mod merge;

pub use apply::*;
use build::*;
pub(crate) use build::{function_paths, string_arguments};
pub use listing::*;
pub use manifest::*;

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
const EXAMPLE_EDITS: &str = include_str!("../../../templates/example-edits.json");
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

/// Report each problem as an error about its mods (for the GUI to mark them), and return one
/// error listing them all.
fn fail(heading: &str, problems: Vec<(Vec<String>, String)>) -> anyhow::Error {
    for (mods, text) in &problems {
        report::send(report::Level::Error, mods, text.clone());
    }
    let texts: Vec<&str> = problems.iter().map(|(_, text)| text.as_str()).collect();
    anyhow::anyhow!("{heading}:\n   {}", texts.join("\n   "))
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
