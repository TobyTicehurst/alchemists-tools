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
//!
//! Every change to the game folder is a [`Transaction`]: each file it replaces or deletes is first
//! moved aside, with a journal written before each step, so a failure part way (a file the running
//! game holds, a full disk) puts back exactly what was there. If the program itself is stopped part
//! way, the next transaction (or [`GameFiles::recover`]) undoes the interrupted one first.

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
/// Where a transaction keeps the files it replaces, and its journal: outside `Paks`, so the game
/// never loads a container kept there.
const TRANSACTION: &str = "AlchemyFactory/.alchemists-transaction";
const JOURNAL: &str = "journal.json";

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
    /// The game functions its C++ code detours, and those it hooks through UE4SS.
    pub detours: Vec<String>,
    pub hooks: Vec<String>,
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

    /// Start a change to the game folder, first undoing one that was interrupted.
    pub fn begin(&self) -> Result<Transaction<'_>> {
        if self.recover()? {
            crate::report::warning!(
                "a change to the game folder was interrupted last time; it has been undone"
            );
        }
        let folder = self.root.join(TRANSACTION);
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("making {}", display_path(&folder)))?;
        let transaction = Transaction {
            files: self,
            folder,
            journal: Journal::default(),
            deleted: vec![],
            finished: false,
        };
        transaction.save_journal()?;
        Ok(transaction)
    }

    /// Undo a transaction that was interrupted (the program stopped part way), if there is one;
    /// returns whether there was.
    pub fn recover(&self) -> Result<bool> {
        let folder = self.root.join(TRANSACTION);
        let journal_path = folder.join(JOURNAL);
        if !journal_path.exists() {
            // a transaction stopped after its journal was removed had finished: only the files it
            // replaced are left
            if folder.exists() {
                std::fs::remove_dir_all(&folder)
                    .with_context(|| format!("removing {}", display_path(&folder)))?;
            }
            return Ok(false);
        }
        let text = std::fs::read_to_string(&journal_path)?;
        let journal: Journal = serde_json::from_str(&text)
            .with_context(|| format!("reading {}", display_path(&journal_path)))?;
        self.undo(&folder, &journal)?;
        std::fs::remove_dir_all(&folder)
            .with_context(|| format!("removing {}", display_path(&folder)))?;
        Ok(true)
    }

    /// Put back what a transaction changed, last change first. Safe to repeat: a file is only put
    /// back while its kept copy exists, and only a file the transaction made is deleted.
    fn undo(&self, folder: &Path, journal: &Journal) -> Result<()> {
        for (index, change) in journal.changes.iter().enumerate().rev() {
            let full = self.resolve(&change.path)?;
            let kept = folder.join(index.to_string());
            if change.existed {
                if kept.exists() {
                    if full.is_file() {
                        std::fs::remove_file(&full)
                            .with_context(|| format!("removing {}", change.path))?;
                    }
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(&kept, &full)
                        .with_context(|| format!("putting back {}", change.path))?;
                }
            } else if full.is_file() {
                std::fs::remove_file(&full).with_context(|| format!("removing {}", change.path))?;
            }
        }
        Ok(())
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

/// What [`Transaction::delete_listed`] did.
#[derive(Default, Debug)]
pub struct Deleted {
    pub deleted: Vec<String>,
    /// Changed since they were installed, so left in place.
    pub kept: Vec<String>,
    pub missing: Vec<String>,
}

/// The files a transaction has touched, in order, written before each is touched.
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    changes: Vec<Change>,
}

#[derive(Serialize, Deserialize)]
struct Change {
    path: String,
    /// Whether there was a file there, which the transaction moved aside (to `<index>` in its
    /// folder) before touching it.
    existed: bool,
}

/// A change to the game folder that happens entirely or not at all (see the module's comment).
/// Dropped without [`Transaction::commit`], as when an error returns early, it undoes itself.
pub struct Transaction<'a> {
    files: &'a GameFiles,
    folder: PathBuf,
    journal: Journal,
    deleted: Vec<String>,
    finished: bool,
}

impl<'a> Transaction<'a> {
    /// The game folder it changes.
    pub fn files(&self) -> &'a GameFiles {
        self.files
    }

    fn save_journal(&self) -> Result<()> {
        let path = self.folder.join(JOURNAL);
        std::fs::write(&path, serde_json::to_string_pretty(&self.journal)?)
            .with_context(|| format!("writing {}", display_path(&path)))
    }

    /// Before a file is replaced or deleted: journal it, then move what's there aside (once per
    /// file). Returns its full path.
    fn keep(&mut self, relative: &str) -> Result<PathBuf> {
        let full = self.files.resolve(relative)?;
        if self
            .journal
            .changes
            .iter()
            .any(|change| change.path == relative)
        {
            return Ok(full);
        }
        if full.exists() && !full.is_file() {
            bail!("{relative} is a folder, not a file");
        }
        let existed = full.exists();
        let index = self.journal.changes.len();
        self.journal.changes.push(Change {
            path: relative.to_string(),
            existed,
        });
        // journaled first, so an interruption between the two can still be undone
        self.save_journal()?;
        if existed {
            std::fs::rename(&full, self.folder.join(index.to_string())).with_context(|| {
                format!("moving {relative} aside to replace it (is the game running?)")
            })?;
        }
        Ok(full)
    }

    /// Put a copy of `source` at `relative`, replacing what's there.
    pub fn copy(&mut self, source: &Path, relative: &str) -> Result<()> {
        let full = self.keep(relative)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, &full).with_context(|| format!("installing {relative}"))?;
        Ok(())
    }

    /// Write `contents` to `relative`, replacing what's there.
    pub fn write(&mut self, relative: &str, contents: &[u8]) -> Result<()> {
        let full = self.keep(relative)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, contents).with_context(|| format!("writing {relative}"))
    }

    /// Delete the file `relative`, if there is one; returns whether there was.
    pub fn delete(&mut self, relative: &str) -> Result<bool> {
        if !self.files.resolve(relative)?.is_file() {
            return Ok(false);
        }
        // moving it aside deletes it
        self.keep(relative)?;
        self.deleted.push(relative.to_string());
        Ok(true)
    }

    /// Delete listed files. Unchanged ones go; changed ones stay unless `force`; missing ones are
    /// reported.
    pub fn delete_listed(&mut self, files: &[FileRecord], force: bool) -> Result<Deleted> {
        let mut out = Deleted::default();
        for file in files {
            let full = self.files.resolve(&file.path)?;
            match self.files.state(file)? {
                State::Missing => out.missing.push(file.path.clone()),
                State::Changed if !force => out.kept.push(file.path.clone()),
                State::Changed if !full.is_file() => out.kept.push(file.path.clone()),
                _ => {
                    self.delete(&file.path)?;
                    out.deleted.push(file.path.clone());
                }
            }
        }
        Ok(out)
    }

    /// Delete files that aren't in any manifest (an uninstall the user forced). Each must still be
    /// a safe path and a file.
    pub fn delete_unlisted(&mut self, relative: &[String]) -> Result<Vec<String>> {
        let mut deleted = vec![];
        for path in relative {
            if self.delete(path)? {
                deleted.push(path.clone());
            }
        }
        Ok(deleted)
    }

    /// Keep every change: drop the journal (from then on there's nothing to undo), the files moved
    /// aside, and the folders the deletions left empty below `ue4ss/Mods/`.
    pub fn commit(mut self) -> Result<()> {
        self.finished = true;
        std::fs::remove_file(self.folder.join(JOURNAL))
            .with_context(|| format!("removing {}", display_path(&self.folder.join(JOURNAL))))?;
        let _ = std::fs::remove_dir_all(&self.folder);
        self.files.remove_empty_folders(&self.deleted);
        Ok(())
    }

    /// Undo every change.
    pub fn roll_back(mut self) -> Result<()> {
        self.finished = true;
        self.files.undo(&self.folder, &self.journal)?;
        std::fs::remove_dir_all(&self.folder)
            .with_context(|| format!("removing {}", display_path(&self.folder)))
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let undone = self
            .files
            .undo(&self.folder, &self.journal)
            .and_then(|_| Ok(std::fs::remove_dir_all(&self.folder)?));
        match undone {
            Ok(()) => crate::report::note!("the game folder is as it was: nothing was changed"),
            // the journal stays, so the next transaction tries again
            Err(error) => crate::report::warning!(
                "couldn't undo every change to the game folder ({error:#}); the next install or \
                 uninstall tries again"
            ),
        }
    }
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
        let mut transaction = files.begin().unwrap();
        let deleted = transaction.delete_listed(&listed, false).unwrap();
        transaction.commit().unwrap();
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
        let mut transaction = files.begin().unwrap();
        let forced = transaction.delete_listed(&listed, true).unwrap();
        transaction.commit().unwrap();
        assert_eq!(forced.deleted, [pak]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_transaction_changes_nothing() {
        let (root, files) = game("roll-back");
        let pak = "AlchemyFactory/Content/Paks/~mods/zzz_Old_P.pak";
        let script = "AlchemyFactory/Binaries/Win64/ue4ss/Mods/Probe/Scripts/main.lua";
        let new = "AlchemyFactory/Content/Paks/~mods/zzz_New_P.pak";
        std::fs::write(root.join(pak), "old").unwrap();
        std::fs::write(root.join(script), "print(1)").unwrap();
        let source = root.join("source.pak");
        std::fs::write(&source, "new").unwrap();
        {
            let mut transaction = files.begin().unwrap();
            transaction.copy(&source, pak).unwrap();
            transaction.copy(&source, new).unwrap();
            transaction.delete(script).unwrap();
            // dropped without commit, as on an error
        }
        assert_eq!(std::fs::read_to_string(root.join(pak)).unwrap(), "old");
        assert_eq!(
            std::fs::read_to_string(root.join(script)).unwrap(),
            "print(1)"
        );
        assert!(!root.join(new).exists());
        assert!(!root.join(TRANSACTION).exists());

        let mut transaction = files.begin().unwrap();
        transaction.copy(&source, pak).unwrap();
        transaction.commit().unwrap();
        assert_eq!(std::fs::read_to_string(root.join(pak)).unwrap(), "new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_interrupted_transaction_is_undone_next_time() {
        let (root, files) = game("recover");
        let pak = "AlchemyFactory/Content/Paks/~mods/zzz_Old_P.pak";
        std::fs::write(root.join(pak), "old").unwrap();
        let source = root.join("source.pak");
        std::fs::write(&source, "new").unwrap();
        let mut transaction = files.begin().unwrap();
        transaction.copy(&source, pak).unwrap();
        std::mem::forget(transaction); // the program stopped here
        assert_eq!(std::fs::read_to_string(root.join(pak)).unwrap(), "new");
        assert!(files.recover().unwrap());
        assert_eq!(std::fs::read_to_string(root.join(pak)).unwrap(), "old");
        assert!(!files.recover().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}
