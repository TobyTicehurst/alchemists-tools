//! UE4SS, which runs mods' Lua and C++ in the game, installed by the tools so that players needn't
//! do it by hand. One build is pinned: C++ mods are compiled against an exact UE4SS build, so every
//! player needs the same one, and a newer experimental build can break with the game. It's checked
//! against its SHA-256 wherever it comes from: the tools' own release (which carries a copy, as
//! UE4SS's MIT licence allows), else UE4SS's own release.
//!
//! It's installed the way mods are: only where no UE4SS is (a UE4SS installed some other way, such
//! as AF Mod Loader's, is left alone, unless it's this very build, which is taken over as it is),
//! in one transaction, with a record of its files
//! (`Binaries/Win64/ue4ss/alchemists-tools-ue4ss.json`) so that removing it deletes only those, and
//! only while they're unchanged. Disabling it renames its loader (`dwmapi.dll`) so the game starts
//! without it, as after a game update that UE4SS can't handle yet.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::files::{FileRecord, GameFiles, State};
use crate::game::{display_path, join, Game};
use crate::report::{note, warning};

/// The pinned build: its name, its zip, and the zip's SHA-256.
pub const VERSION: &str = "v3.0.1-1164-g5e627997";
pub const ZIP: &str = "UE4SS_v3.0.1-1164-g5e627997.zip";
const ZIP_SHA256: &str = "3c72142a383e25e9f9553ac9e4d5677aa572e98cb369cac57200efb2e381e603";
/// Its `UE4SS.dll`, to recognise the build however it was installed.
const DLL_SHA256: &str = "87b494191b7bc9bcfd938f54190c09e9aaa01dc354f6927ba661b0a18bad13ee";
/// Where the zip is downloaded from, in turn: the tools' release of this version, then UE4SS's.
fn sources() -> [String; 2] {
    [
        format!(
            "https://github.com/TobyTicehurst/alchemists-tools/releases/download/v{}/{ZIP}",
            env!("CARGO_PKG_VERSION")
        ),
        format!("https://github.com/UE4SS-RE/RE-UE4SS/releases/download/experimental/{ZIP}"),
    ]
}
const LIMIT: u64 = 64 << 20;
/// Where UE4SS goes, relative to the game folder: its loader here, everything else in `ue4ss/`.
const BINARIES: &str = "AlchemyFactory/Binaries/Win64";
/// The loaders UE4SS uses: the game loads whichever is beside it.
const LOADERS: &[&str] = &["dwmapi.dll", "xinput1_3.dll"];
const DISABLED: &str = ".disabled";
const RECORD: &str = "AlchemyFactory/Binaries/Win64/ue4ss/alchemists-tools-ue4ss.json";

/// What the tools installed (or took over): the build and its files as installed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Record {
    version: String,
    files: Vec<FileRecord>,
}

/// UE4SS in a game folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    None,
    /// Installed by the tools.
    Tools {
        version: String,
        disabled: bool,
    },
    /// Installed some other way: whether it's the pinned build, and its version as its log says.
    Other {
        pinned: bool,
        version: Option<String>,
        disabled: bool,
    },
}

fn binaries(game: &Game) -> PathBuf {
    join(&game.root, &["AlchemyFactory", "Binaries", "Win64"])
}

/// The loader beside the game, or its disabled copy: (its name, whether it's disabled).
fn loader(game: &Game) -> Option<(&'static str, bool)> {
    let folder = binaries(game);
    LOADERS.iter().find_map(|name| {
        if folder.join(name).is_file() {
            Some((*name, false))
        } else if folder.join(format!("{name}{DISABLED}")).is_file() {
            Some((*name, true))
        } else {
            None
        }
    })
}

/// Whether UE4SS's loader is beside the game, enabled or not.
pub fn has_loader(game: &Game) -> bool {
    loader(game).is_some()
}

fn read_record(game: &Game) -> Option<Record> {
    let text = std::fs::read_to_string(game.root.join(RECORD)).ok()?;
    serde_json::from_str(&text).ok()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether, and how, UE4SS is in the game folder.
pub fn installed(game: &Game) -> Installed {
    let folder = binaries(game);
    let dll = ["ue4ss/UE4SS.dll", "UE4SS.dll"]
        .iter()
        .map(|dll| folder.join(dll))
        .find(|path| path.is_file());
    let Some(dll) = dll else {
        return Installed::None;
    };
    let disabled = loader(game).is_some_and(|(_, disabled)| disabled);
    if let Some(record) = read_record(game) {
        return Installed::Tools {
            version: record.version,
            disabled,
        };
    }
    let pinned = crate::files::hash(&dll).is_ok_and(|(_, hash)| hash == DLL_SHA256);
    Installed::Other {
        pinned,
        version: crate::environment::ue4ss_version(game),
        disabled,
    }
}

/// Download `url` with HTTPS only, through the system's curl (Windows' own, on Windows).
fn download(url: &str) -> Result<Vec<u8>> {
    let curl = match std::env::var_os("SystemRoot") {
        Some(root) => Path::new(&root).join("System32").join("curl.exe"),
        None => PathBuf::from("curl"),
    };
    let file = std::env::temp_dir().join(format!("alchemists-tools-{ZIP}"));
    let mut command = std::process::Command::new(&curl);
    command
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-filesize", &LIMIT.to_string(), "--output"])
        .arg(&file)
        .arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .with_context(|| format!("running {}", display_path(&curl)))?;
    if !output.status.success() {
        bail!(
            "couldn't download {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let bytes = std::fs::read(&file)?;
    let _ = std::fs::remove_file(&file);
    Ok(bytes)
}

/// The pinned build's zip: the one given, or downloaded; checked against its SHA-256 either way.
fn zip_bytes(given: Option<&Path>) -> Result<Vec<u8>> {
    if let Some(path) = given {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading {}", display_path(path)))?;
        if sha256(&bytes) != ZIP_SHA256 {
            bail!(
                "{} isn't UE4SS {VERSION} (its SHA-256 differs): the tools install only that build",
                display_path(path)
            );
        }
        return Ok(bytes);
    }
    let mut failures = vec![];
    for url in sources() {
        note!("downloading {url}");
        match download(&url) {
            Ok(bytes) if sha256(&bytes) == ZIP_SHA256 => return Ok(bytes),
            Ok(_) => failures.push(format!(
                "{url}: not the expected file (its SHA-256 differs)"
            )),
            Err(error) => failures.push(format!("{error:#}")),
        }
    }
    bail!(
        "couldn't get UE4SS {VERSION}:\n   {}",
        failures.join("\n   ")
    )
}

/// The zip's files, with where each goes (relative to the game folder): its loader beside the
/// game, the rest below `ue4ss/`. Any other path is refused.
fn zip_files(bytes: Vec<u8>) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut files = vec![];
    for index in 0..zip.len() {
        let mut file = zip.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        let plain = file.enclosed_name().is_some()
            && !name.contains(['\\', ':'])
            && name
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        if !plain || !(LOADERS.contains(&name.as_str()) || name.starts_with("ue4ss/")) {
            bail!("UE4SS's zip holds {name:?}, which isn't one of its files");
        }
        let mut contents = vec![];
        (&mut file).take(LIMIT).read_to_end(&mut contents)?;
        files.push((format!("{BINARIES}/{name}"), contents));
    }
    Ok(files)
}

/// Install the pinned UE4SS into the game folder (see the module's comment). `zip`: the build's zip,
/// if it's at hand, instead of downloading it.
pub fn install(game: &Game, zip: Option<&Path>) -> Result<()> {
    crate::mods::check_game_closed()?;
    let files = GameFiles::new(&game.root)?;
    match installed(game) {
        Installed::Tools { version, .. } if version == VERSION => {
            note!("UE4SS {VERSION} is installed already");
            return Ok(());
        }
        Installed::Tools { version, .. } => {
            note!("replacing UE4SS {version} with {VERSION}");
        }
        Installed::Other {
            pinned: false,
            version,
            ..
        } => bail!(
            "a UE4SS the tools didn't install is in the game folder already ({}), so it's left \
             alone. Mods with C++ made for these tools need UE4SS {VERSION}: to use it, remove \
             the other (its dwmapi.dll and ue4ss folder) and install again",
            version.as_deref().unwrap_or("its version unknown")
        ),
        Installed::Other { pinned: true, .. } => {
            note!("UE4SS {VERSION} is installed already, by hand: the tools take it over as it is");
        }
        Installed::None => {}
    }
    let previous = read_record(game);
    let wanted = zip_files(zip_bytes(zip)?)?;
    let ours = |relative: &str| {
        previous
            .as_ref()
            .and_then(|record| record.files.iter().find(|file| file.path == relative))
            .cloned()
    };
    // it never writes over a file that isn't its own: one that's there already is either the
    // same file (taken over) or this tool's, unchanged
    let mut problems = vec![];
    let mut write = vec![];
    let mut taken_over = vec![];
    for (relative, contents) in &wanted {
        let full = files.resolve(relative)?;
        if !full.exists() {
            write.push((relative.clone(), contents));
            continue;
        }
        let current = files.record(relative)?;
        if current.sha256 == sha256(contents) {
            taken_over.push(current);
            continue;
        }
        match ours(relative) {
            Some(old) if files.state(&old)? == State::Unchanged => {
                write.push((relative.clone(), contents));
            }
            Some(_) => {
                warning!("{relative} has changed since it was installed, so it's kept as it is");
            }
            None => problems.push(format!(
                "{relative} is there already, and isn't UE4SS's own"
            )),
        }
    }
    if !problems.is_empty() {
        bail!(
            "can't install UE4SS {VERSION}: it would replace files that aren't its own:\n   {}",
            problems.join("\n   ")
        );
    }
    let mut transaction = files.begin()?;
    let mut recorded = taken_over;
    for (relative, contents) in write {
        transaction.write(&relative, contents)?;
        recorded.push(files.record(&relative)?);
    }
    // the old build's files the new one doesn't have
    if let Some(previous) = &previous {
        let gone: Vec<FileRecord> = previous
            .files
            .iter()
            .filter(|old| !wanted.iter().any(|(relative, _)| *relative == old.path))
            .cloned()
            .collect();
        transaction.delete_listed(&gone, false)?;
    }
    let record = Record {
        version: VERSION.to_string(),
        files: recorded,
    };
    transaction.write(
        RECORD,
        (serde_json::to_string_pretty(&record)? + "\n").as_bytes(),
    )?;
    transaction.commit()?;
    note!(
        "installed UE4SS {VERSION} in {}: mods with Lua or C++ can be installed",
        display_path(&binaries(game))
    );
    Ok(())
}

/// Remove the UE4SS the tools installed: only its files, and only those unchanged (`force`:
/// changed ones too). The mods that need it stay installed, but don't run without it.
pub fn remove(game: &Game, force: bool) -> Result<()> {
    crate::mods::check_game_closed()?;
    let Some(record) = read_record(game) else {
        bail!(
            "the tools didn't install the UE4SS in {} (it has no record of it), so they don't \
             remove it",
            display_path(&binaries(game))
        );
    };
    let files = GameFiles::new(&game.root)?;
    let mut transaction = files.begin()?;
    let mut listed = record.files.clone();
    // a disabled loader is removed under its disabled name
    for file in &mut listed {
        let disabled = format!("{}{DISABLED}", file.path);
        let is_loader = LOADERS
            .iter()
            .any(|name| file.path == format!("{BINARIES}/{name}"));
        if is_loader && !files.resolve(&file.path)?.exists() && files.resolve(&disabled)?.exists() {
            file.path = disabled;
        }
    }
    let result = transaction.delete_listed(&listed, force)?;
    transaction.delete(RECORD)?;
    transaction.commit()?;
    // the folders it leaves empty (not those holding logs or mods)
    let folder = binaries(game).join("ue4ss");
    for empty in ["UE4SS_SDK_Backends", "Mods/shared", "Mods", ""] {
        let _ = std::fs::remove_dir(folder.join(empty));
    }
    for kept in &result.kept {
        note!("   kept {kept}: it has changed since UE4SS was installed (force deletes it)");
    }
    note!("removed UE4SS {}", record.version);
    let mods = crate::mods::installed_record(game)?;
    let needing: Vec<&str> = mods
        .mods
        .values()
        .filter(|entry| entry.needs_ue4ss)
        .map(|entry| entry.name.as_str())
        .collect();
    if !needing.is_empty() {
        warning!(
            "{} need UE4SS, so they don't run until it's installed again",
            needing.join(", ")
        );
    }
    Ok(())
}

/// Disable UE4SS, or enable it again, by renaming its loader beside the game (`dwmapi.dll`), so the
/// game starts without it. Works for any UE4SS, however it was installed.
pub fn set_enabled(game: &Game, enable: bool) -> Result<()> {
    crate::mods::check_game_closed()?;
    let Some((name, disabled)) = loader(game) else {
        bail!("there's no UE4SS loader (dwmapi.dll) beside the game");
    };
    let folder = binaries(game);
    let (from, to) = if enable {
        (format!("{name}{DISABLED}"), name.to_string())
    } else {
        (name.to_string(), format!("{name}{DISABLED}"))
    };
    // renamed only if it isn't that way already
    if disabled == enable {
        std::fs::rename(folder.join(&from), folder.join(&to))
            .with_context(|| format!("renaming {from} to {to}"))?;
    }
    note!(
        "UE4SS is {}: the game starts {} it",
        if enable { "enabled" } else { "disabled" },
        if enable { "with" } else { "without" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip(names: &[&str]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
        for name in names {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(name.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn only_ue4ss_files_are_placed() {
        let files = zip_files(zip(&[
            "dwmapi.dll",
            "ue4ss/UE4SS.dll",
            "ue4ss/Mods/mods.txt",
        ]))
        .unwrap();
        assert_eq!(files[0].0, "AlchemyFactory/Binaries/Win64/dwmapi.dll");
        assert_eq!(
            files[2].0,
            "AlchemyFactory/Binaries/Win64/ue4ss/Mods/mods.txt"
        );
        for bad in [
            "version.dll",
            "../x.dll",
            "ue4ss/../../x",
            "other/UE4SS.dll",
        ] {
            assert!(zip_files(zip(&[bad])).is_err(), "{bad} was placed");
        }
    }

    /// A stand-in game folder with the pinned build, installed, disabled and removed again.
    #[test]
    #[ignore = "needs UE4SS's zip: set ALCHEMISTS_UE4SS_ZIP to it"]
    fn install_disable_remove() {
        let Some(zip) = std::env::var_os("ALCHEMISTS_UE4SS_ZIP") else {
            return;
        };
        let root = std::env::temp_dir().join(format!("alchemists-ue4ss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("AlchemyFactory/Binaries/Win64")).unwrap();
        std::fs::create_dir_all(root.join("AlchemyFactory/Content/Paks")).unwrap();
        let game = Game::new(&root);
        install(&game, Some(Path::new(&zip))).unwrap();
        assert!(matches!(
            installed(&game),
            Installed::Tools {
                disabled: false,
                ..
            }
        ));
        set_enabled(&game, false).unwrap();
        assert!(matches!(
            installed(&game),
            Installed::Tools { disabled: true, .. }
        ));
        remove(&game, false).unwrap();
        assert_eq!(installed(&game), Installed::None);
        assert!(!root.join("AlchemyFactory/Binaries/Win64/ue4ss").exists());
        assert!(!root
            .join("AlchemyFactory/Binaries/Win64/dwmapi.dll.disabled")
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
