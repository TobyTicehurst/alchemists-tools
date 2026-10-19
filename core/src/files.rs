//! The files mods put in the game folder, and the checks that make deleting or replacing one safe.
//!
//! Every installed mod has a manifest (`Paks/~mods/manifests/<name>.json`) listing each of its
//! files with its size and SHA-256 hash, and no two mods may own the same file. A file is only
//! deleted or replaced when all of this holds:
//! 1. it's listed for that purpose (in the mod's manifest, or as a file the tool generated);
//! 2. its path is relative, has no `..`, starts with `AlchemyFactory/`, and resolves inside the
//!    game folder (symbolic links followed);
//! 3. its size and hash are still those that were installed (a changed file is left alone, unless
//!    forced).
//!
//! Folders are never deleted with their contents: only files, one by one, and then the folders they
//! leave empty below `ue4ss/Mods/` (never `Mods` or `shared` themselves).

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::game::display_path;

/// Where manifests are, relative to the game folder.
pub const MANIFESTS: &str = "AlchemyFactory/Content/Paks/~mods/manifests";
/// Below this, folders a mod's files leave empty are removed (a UE4SS mod's own folder).
const UE4SS_MODS: &str = "AlchemyFactory/Binaries/Win64/ue4ss/Mods";

/// One file a mod (or the tool) put in the game folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Relative to the game folder, with `/`.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// What a mod installed: it names itself and lists every file it owns.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Manifest {
    pub name: String,
    pub title: String,
    pub version: String,
    pub game_build: Option<String>,
    pub dependencies: Vec<String>,
    /// `alchemists-tools` for `mod install`; `zip` for a mod extracted from its zip, whose own data
    /// container and template save are among its files.
    pub installed_by: String,
    /// The game tables its edits change.
    pub tables: Vec<String>,
    pub new_buildings: Vec<String>,
    pub files: Vec<FileRecord>,
}

/// A listed file as it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Size and hash as installed.
    Unchanged,
    /// Something else has changed or replaced it since.
    Changed,
    Missing,
}

/// The SHA-256 of a file, as lowercase hex, and its size.
pub fn hash(path: &Path) -> Result<(u64, String)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", display_path(path)))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let mut size = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    let digest = hasher.finalize();
    Ok((
        size,
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    ))
}

/// The game folder, for checked file operations.
pub struct GameFiles {
    root: PathBuf,
    canonical: PathBuf,
}

impl GameFiles {
    pub fn new(root: &Path) -> Result<Self> {
        let canonical = root
            .canonicalize()
            .with_context(|| format!("finding {}", display_path(root)))?;
        Ok(GameFiles {
            root: root.to_path_buf(),
            canonical,
        })
    }

    /// The full path of a file given relative to the game folder, if it's safe to touch: relative,
    /// only plain names (no `..`, `.`, drive or root), below `AlchemyFactory/`, and inside the game
    /// folder once resolved (so a link can't lead outside it).
    pub fn resolve(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        // Path::components drops a `.` in the middle, so the text's own segments are checked too
        let plain = path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
            && relative
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | ".."));
        if !plain || relative.contains('\\') || !relative.starts_with("AlchemyFactory/") {
            bail!(
                "refusing the path {relative:?}: not a plain path inside the game's \
                AlchemyFactory folder"
            );
        }
        let full = self.root.join(path);
        // the deepest part that exists must resolve inside the game folder
        let mut existing = full.as_path();
        while !existing.exists() {
            existing = existing.parent().context("no existing parent")?;
        }
        let resolved = existing.canonicalize()?;
        if !resolved.starts_with(&self.canonical) {
            bail!(
                "refusing {relative:?}: it resolves to {}, outside the game folder",
                display_path(&resolved)
            );
        }
        Ok(full)
    }

    /// A record of a file as it is now.
    pub fn record(&self, relative: &str) -> Result<FileRecord> {
        let (size, sha256) = hash(&self.resolve(relative)?)?;
        Ok(FileRecord {
            path: relative.to_string(),
            size,
            sha256,
        })
    }

    pub fn state(&self, file: &FileRecord) -> Result<State> {
        let full = self.resolve(&file.path)?;
        if !full.exists() {
            return Ok(State::Missing);
        }
        if !full.is_file() {
            return Ok(State::Changed); // a folder or something else where the file was
        }
        let (size, sha256) = hash(&full)?;
        Ok(if size == file.size && sha256 == file.sha256 {
            State::Unchanged
        } else {
            State::Changed
        })
    }

    /// Delete listed files. Unchanged ones go; changed ones stay unless `force`; missing ones are
    /// reported. Then the folders this emptied below `ue4ss/Mods/` are removed.
    pub fn delete_listed(&self, files: &[FileRecord], force: bool) -> Result<Deleted> {
        let mut out = Deleted::default();
        for file in files {
            let full = self.resolve(&file.path)?;
            match self.state(file)? {
                State::Missing => out.missing.push(file.path.clone()),
                State::Changed if !force => out.kept.push(file.path.clone()),
                State::Changed if !full.is_file() => out.kept.push(file.path.clone()),
                _ => {
                    std::fs::remove_file(&full)
                        .with_context(|| format!("deleting {}", display_path(&full)))?;
                    out.deleted.push(file.path.clone());
                }
            }
        }
        self.remove_empty_folders(&out.deleted);
        Ok(out)
    }

    /// Delete files that aren't in any manifest (an uninstall the user forced). Each must still be
    /// a safe path and a file; folders are only removed when left empty.
    pub fn delete_unlisted(&self, relative: &[String]) -> Result<Vec<String>> {
        let mut deleted = vec![];
        for path in relative {
            let full = self.resolve(path)?;
            if full.is_file() {
                std::fs::remove_file(&full)
                    .with_context(|| format!("deleting {}", display_path(&full)))?;
                deleted.push(path.clone());
            }
        }
        self.remove_empty_folders(&deleted);
        Ok(deleted)
    }

    /// Remove the folders that deleting `files` left empty, below `ue4ss/Mods/<mod>/` (or
    /// `Mods/shared/<module>/`); never anywhere else.
    fn remove_empty_folders(&self, files: &[String]) {
        for file in files {
            let mut folder = Path::new(file).parent();
            while let Some(current) = folder {
                let text = current.to_string_lossy().replace('\\', "/");
                let inside = text
                    .strip_prefix(&format!("{UE4SS_MODS}/"))
                    .is_some_and(|rest| rest != "shared");
                if !inside {
                    break;
                }
                let full = self.root.join(current);
                let empty = std::fs::read_dir(&full)
                    .map(|mut entries| entries.next().is_none())
                    .unwrap_or(false);
                if !empty || std::fs::remove_dir(&full).is_err() {
                    break;
                }
                folder = current.parent();
            }
        }
    }

    /// The manifests in the game folder, by mod name.
    pub fn manifests(&self) -> Result<Vec<(String, Manifest)>> {
        let folder = self.root.join(MANIFESTS);
        let mut out = vec![];
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        for path in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let text = std::fs::read_to_string(&path)?;
            let manifest: Manifest = serde_json::from_str(&text)
                .with_context(|| format!("reading {}", display_path(&path)))?;
            out.push((
                path.file_stem().unwrap().to_string_lossy().into_owned(),
                manifest,
            ));
        }
        Ok(out)
    }

    pub fn manifest_path(name: &str) -> String {
        format!("{MANIFESTS}/{name}.json")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// What [`GameFiles::delete_listed`] did.
#[derive(Default, Debug)]
pub struct Deleted {
    pub deleted: Vec<String>,
    /// Changed since they were installed, so left in place.
    pub kept: Vec<String>,
    pub missing: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(test: &str) -> (PathBuf, GameFiles) {
        let root =
            std::env::temp_dir().join(format!("alchemists-files-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(
            root.join("AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/Scripts"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("AlchemyFactory/Content/Paks/~mods")).unwrap();
        let files = GameFiles::new(&root).unwrap();
        (root, files)
    }

    #[test]
    fn unsafe_paths_are_refused() {
        let (root, files) = game("paths");
        for bad in [
            "../x",
            "AlchemyFactory/../../x",
            "/AlchemyFactory/x",
            "C:/x",
            "Other/x",
            "AlchemyFactory\\x",
            "AlchemyFactory/./x",
        ] {
            assert!(files.resolve(bad).is_err(), "{bad} was allowed");
        }
        assert!(files
            .resolve("AlchemyFactory/Content/Paks/~mods/x.pak")
            .is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_unchanged_listed_files_are_deleted() {
        let (root, files) = game("delete");
        let script = "AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/Scripts/main.lua";
        let pak = "AlchemyFactory/Content/Paks/~mods/zzz_Probe_P.pak";
        std::fs::write(root.join(script), "print(1)").unwrap();
        std::fs::write(root.join(pak), "pak").unwrap();
        let listed = vec![files.record(script).unwrap(), files.record(pak).unwrap()];
        std::fs::write(root.join(pak), "changed").unwrap(); // someone replaced it
        std::fs::write(
            root.join("AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/notes.txt"),
            "mine",
        )
        .unwrap();
        let deleted = files.delete_listed(&listed, false).unwrap();
        assert_eq!(deleted.deleted, [script]);
        assert_eq!(deleted.kept, [pak]);
        assert!(root.join(pak).exists(), "a changed file was deleted");
        // the user's own file keeps the mod's folder, which isn't emptied
        assert!(root
            .join("AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/notes.txt")
            .exists());
        assert!(!root
            .join("AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/Scripts")
            .exists());
        assert!(root
            .join("AlchemyFactory/Binaries/Win64/ue4ss/Mods")
            .exists());
        let forced = files.delete_listed(&listed, true).unwrap();
        assert_eq!(forced.deleted, [pak]);
        let _ = std::fs::remove_dir_all(root);
    }
}
