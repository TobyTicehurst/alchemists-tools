//! What's in the folders of mods and the game folder, for listing; mods kept as zips.

use super::*;

impl Workspace {
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
                    needs_ue4ss: folder.join("ue4ss").is_dir()
                        || folder.join("ue4ss-shared").is_dir(),
                    changes_tables: !found.edits.is_empty() || !found.optional_edits.is_empty(),
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
        let ue4ss_prefix = format!("{}/", UE4SS_FOLDER.join("/"));
        let uses_ue4ss = |manifest: &Manifest| {
            manifest
                .files
                .iter()
                .any(|file| file.path.starts_with(&ue4ss_prefix))
        };
        for (name, archive) in self.archives() {
            let manifest = &archive.manifest;
            let summary = mods.entry(name.clone()).or_insert_with(|| Summary {
                title: manifest.title.clone(),
                version: manifest.version.clone(),
                homepage: manifest.homepage.clone(),
                dependencies: manifest.dependencies.clone(),
                game_build: manifest.game_build.clone(),
                detours: manifest.detours.clone(),
                hooks: manifest.hooks.clone(),
                tables: manifest.tables.clone(),
                new_buildings: manifest.new_buildings.clone(),
                cautions: manifest.cautions.clone(),
                needs_ue4ss: uses_ue4ss(manifest),
                changes_tables: !manifest.tables.is_empty(),
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
                homepage: manifest.homepage.clone(),
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
            summary.needs_ue4ss |= uses_ue4ss(manifest);
            summary.changes_tables |= !manifest.tables.is_empty();
        }
        for (name, entry) in &record.mods {
            let summary = mods.entry(name.clone()).or_insert_with(|| Summary {
                title: entry.title.clone(),
                version: entry.version.clone(),
                homepage: entry.homepage.clone(),
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
                summary.needs_ue4ss |= entry.needs_ue4ss;
                summary.changes_tables |= !entry.tables.is_empty();
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
        let current = game_build(&self.game);
        Ok(Overview {
            mods: mods.into_values().collect(),
            unmanaged,
            game_updated: record.game_updated(current.as_deref()),
            game_build: current,
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
    /// The game's mods on Nexus Mods whose name or summary has `search` in it (all if None), most
    /// endorsed first, and which of them are in the folders of mods or installed.
    pub fn browse(&self, search: Option<&str>) -> Result<()> {
        let overview = self.overview()?;
        let search = search.map(str::to_lowercase);
        let found = crate::nexus::mods()?;
        let mut shown = 0;
        for nexus in &found {
            let matches = search.as_ref().is_none_or(|search| {
                nexus.name.to_lowercase().contains(search)
                    || nexus.summary.to_lowercase().contains(search)
            });
            if !matches {
                continue;
            }
            shown += 1;
            let here = match overview.from_nexus(nexus) {
                Some(summary) if summary.installed.is_some() => {
                    format!(" [installed: {}]", summary.name)
                }
                Some(summary) => format!(" [in the mods folders: {}]", summary.name),
                None => String::new(),
            };
            note!(
                "{} {} by {}: {} endorsements, {} downloads, updated {}{here}\n   {}\n   {}",
                nexus.name,
                nexus.version,
                nexus.author,
                nexus.endorsements,
                nexus.downloads,
                nexus.updated,
                nexus.summary,
                nexus.page()
            );
        }
        note!(
            "{shown} of the {} mods on Nexus Mods; to install one, download its zip from its page \
             and import it (mod import <zip>), or put it in a mods folder if it was made with \
             these tools",
            found.len()
        );
        Ok(())
    }

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
        if let Some((installed, current)) = &overview.game_updated {
            warning!("{}", game_updated_text(installed, current));
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
    pub(super) fn entry(&self) -> Entry {
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
pub(super) fn read_archive(path: &Path) -> Option<Archive> {
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
    /// Whether it has Lua or C++ that UE4SS runs, and whether it changes the game's tables: known
    /// before it's installed, from its folder or zip.
    pub needs_ue4ss: bool,
    pub changes_tables: bool,
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
    /// (the build the installed mods were installed on, the game's build now), if the game has
    /// updated since ([`Record::game_updated`]).
    pub game_updated: Option<(String, String)>,
    pub load_order: Vec<String>,
}

impl Overview {
    pub fn get(&self, name: &str) -> Option<&Summary> {
        self.mods.iter().find(|summary| summary.name == name)
    }

    /// The mod here that's this one on Nexus: the one whose homepage is its page, or else one
    /// with its name (ignoring case, spaces and punctuation).
    pub fn from_nexus(&self, nexus: &crate::nexus::NexusMod) -> Option<&Summary> {
        let simple = |text: &str| -> String {
            text.chars()
                .filter(|character| character.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect()
        };
        let name = simple(&nexus.name);
        self.mods
            .iter()
            .find(|summary| crate::nexus::mod_id_in(&summary.homepage) == Some(nexus.id))
            .or_else(|| {
                self.mods.iter().find(|summary| {
                    !name.is_empty()
                        && (simple(&summary.name) == name || simple(&summary.title) == name)
                })
            })
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

/// What a game update since the mods were installed means, and what to do (for `mod list`).
fn game_updated_text(installed: &str, current: &str) -> String {
    format!(
        "the game has updated (build {installed} to {current}) since the mods were installed: \
         install them again (mod apply, or the GUI's Confirm) so their table changes are merged \
         into the new game's tables, which until then hide the update's own; mods' C++ and UE4SS \
         may not work with the new build yet: if the game crashes at start, disable UE4SS \
         (alchemists-tools ue4ss disable)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
