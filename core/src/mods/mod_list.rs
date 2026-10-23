//! Mod lists, for playing together: everyone in a multiplayer game needs the same mods. A list
//! names the installed mods with their versions, homepages, and a checksum of the files each
//! installed (not the merged tables, which depend on the other mods); comparing one with what's
//! here says what to install, update or uninstall.

use sha2::{Digest, Sha256};

use super::*;

/// What the list's file says it is.
const KIND: &str = "alchemists-tools mod list";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct ModList {
    pub kind: String,
    /// The game's build where it was made.
    pub game_build: Option<String>,
    pub mods: Vec<Listed>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Listed {
    pub name: String,
    pub title: String,
    pub version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    /// The SHA-256 of its installed files' paths and SHA-256s (None for a mod installed without a
    /// manifest).
    pub checksum: Option<String>,
}

/// A list compared with what's here.
#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// The list's mods that are here, to tick: installing exactly these matches the list as far as
    /// it can.
    pub wanted: Vec<String>,
    /// What differs, each said for a player; none if what's installed matches.
    pub differences: Vec<String>,
}

impl ModList {
    pub fn read(path: &Path) -> Result<Self> {
        let list: ModList = serde_json::from_value(read_json(path)?)
            .with_context(|| format!("{} isn't a mod list", display_path(path)))?;
        if list.kind != KIND {
            bail!("{} isn't a mod list", display_path(path));
        }
        Ok(list)
    }
}

/// The checksum of the files a manifest lists, and of the edits merged for the mod (its install
/// record entry's), which are all there is of a mod that only changes tables.
fn checksum(manifest: &Manifest, entry: Option<&Entry>) -> String {
    let mut files: Vec<&FileRecord> = manifest.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(format!("{}\0{}\n", file.path, file.sha256).as_bytes());
    }
    if let Some(entry) = entry {
        let edits = json!([entry.edits, entry.optional_edits, entry.save_edits]);
        hasher.update(edits.to_string().as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Workspace {
    /// The installed mods, as a list.
    pub fn mod_list(&self) -> Result<ModList> {
        let files = GameFiles::new(&self.game.root)?;
        let manifests = files.manifests()?;
        let record = self.record()?;
        let overview = self.overview()?;
        let mut mods: Vec<Listed> = overview
            .installed()
            .map(|summary| {
                let installed = summary.installed.as_ref();
                Listed {
                    name: summary.name.clone(),
                    title: summary.title.clone(),
                    version: installed.map_or(summary.version.clone(), |installed| {
                        installed.version.clone()
                    }),
                    homepage: summary.homepage.clone(),
                    checksum: manifests
                        .iter()
                        .find(|(name, _)| *name == summary.name)
                        .map(|(_, manifest)| checksum(manifest, record.mods.get(&summary.name))),
                }
            })
            .collect();
        mods.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ModList {
            kind: KIND.to_string(),
            game_build: overview.game_build,
            mods,
        })
    }

    /// Write the installed mods' list to `path`.
    pub fn export_list(&self, path: &Path) -> Result<ModList> {
        let list = self.mod_list()?;
        write_json(path, &list)?;
        note!(
            "wrote the list of the {} installed mod(s) to {}: the players you play with compare \
             theirs with it (the GUI's Load a mod list, or mod compare-list)",
            list.mods.len(),
            display_path(path)
        );
        Ok(list)
    }

    /// Compare a list with what's here.
    pub fn compare_list(&self, list: &ModList) -> Result<Comparison> {
        let here = self.mod_list()?;
        let overview = self.overview()?;
        let mut comparison = Comparison::default();
        if let (Some(theirs), Some(ours)) = (&list.game_build, &here.game_build) {
            if theirs != ours {
                comparison.differences.push(format!(
                    "the list was made on game build {theirs}, and the game here is build {ours}: \
                     everyone needs the same game version"
                ));
            }
        }
        for listed in &list.mods {
            let named = format!("{} {}", listed.title, listed.version);
            let Some(summary) = overview.get(&listed.name) else {
                let from = if listed.homepage.is_empty() {
                    String::new()
                } else {
                    format!(" (from {})", listed.homepage)
                };
                comparison.differences.push(format!(
                    "{named}: not in your mods folders; download it{from}, then load the list again"
                ));
                continue;
            };
            comparison.wanted.push(listed.name.clone());
            // what Confirm would install: the folder's version, or the installed one
            let version = match (&summary.folder, &summary.installed) {
                (None, Some(installed)) => &installed.version,
                _ => &summary.version,
            };
            if *version != listed.version {
                comparison.differences.push(format!(
                    "{}: you have version {version}, and the list has {}",
                    listed.title, listed.version
                ));
                continue;
            }
            let ours = here.mods.iter().find(|mine| mine.name == listed.name);
            match (
                ours.and_then(|mine| mine.checksum.as_ref()),
                &listed.checksum,
            ) {
                (Some(ours), Some(theirs)) if ours != theirs => {
                    comparison.differences.push(format!(
                        "{named}: the same version, but its installed files differ from the \
                         list's (a different build of it?)"
                    ));
                }
                _ if ours.is_none() => comparison
                    .differences
                    .push(format!("{named}: not installed (Confirm installs it)")),
                _ => {}
            }
        }
        for mine in &here.mods {
            if !list.mods.iter().any(|listed| listed.name == mine.name) {
                comparison.differences.push(format!(
                    "{} {}: installed, but not in the list (Confirm uninstalls it)",
                    mine.title, mine.version
                ));
            }
        }
        Ok(comparison)
    }

    /// Compare the list at `path` with what's here, and say what differs.
    pub fn compare_list_file(&self, path: &Path) -> Result<()> {
        let comparison = self.compare_list(&ModList::read(path)?)?;
        if comparison.differences.is_empty() {
            note!("the installed mods match the list");
            return Ok(());
        }
        for difference in &comparison.differences {
            note!("{difference}");
        }
        if !comparison.wanted.is_empty() {
            note!(
                "to install the list's mods that are here (and uninstall the others): \
                 alchemists-tools mod apply {}",
                comparison.wanted.join(" ")
            );
        }
        Ok(())
    }
}
