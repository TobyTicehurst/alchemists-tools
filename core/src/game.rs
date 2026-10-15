//! Locating the game install.

use std::path::{Path, PathBuf};

pub const STEAM_FOLDER: &str = "Alchemy Factory";

/// `base` joined with each folder in turn, so the path uses the platform's separator throughout.
pub fn join(base: &Path, folders: &[&str]) -> PathBuf {
    folders
        .iter()
        .fold(base.to_path_buf(), |path, folder| path.join(folder))
}

/// A path made absolute and in the platform's own form, for messages.
pub fn display_path(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// A game install: the directory containing `AlchemyFactory/`.
pub struct Game {
    pub root: PathBuf,
}

impl Game {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Game { root: root.into() }
    }

    /// Find the game in the Steam libraries.
    pub fn find() -> Option<Self> {
        steam_libraries()
            .into_iter()
            .map(|library| join(&library, &["steamapps", "common", STEAM_FOLDER]))
            .find(|path| path.join("AlchemyFactory").is_dir())
            .map(Game::new)
    }

    pub fn paks(&self) -> PathBuf {
        join(&self.root, &["AlchemyFactory", "Content", "Paks"])
    }

    pub fn pak(&self) -> PathBuf {
        self.paks().join("AlchemyFactory-Windows.pak")
    }

    /// The newest `.usmap` dumped by UE4SS into the game's `Binaries/Win64/ue4ss/` (or `Win64/`).
    pub fn find_usmap(&self) -> Option<PathBuf> {
        let binaries = join(&self.root, &["AlchemyFactory", "Binaries", "Win64"]);
        [binaries.join("ue4ss"), binaries]
            .iter()
            .filter_map(|directory| std::fs::read_dir(directory).ok())
            .flatten()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("usmap"))
            })
            .max_by_key(|entry| {
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
            })
            .map(|entry| entry.path())
    }
}

/// Where Steam is installed, as recorded in the registry by its installer.
#[cfg(windows)]
fn registered_steam() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let entries = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    entries
        .iter()
        .filter_map(|(root, key, value)| {
            let path: String = RegKey::predef(*root)
                .open_subkey(key)
                .ok()?
                .get_value(value)
                .ok()?;
            // SteamPath is written with forward slashes, e.g. `c:/program files (x86)/steam`.
            Some(PathBuf::from(path.replace('/', "\\")))
        })
        .collect()
}

#[cfg(not(windows))]
fn registered_steam() -> Vec<PathBuf> {
    vec![]
}

fn steam_libraries() -> Vec<PathBuf> {
    let drive = Path::new("C:\\");
    let mut roots = registered_steam();
    roots.push(join(drive, &["Program Files (x86)", "Steam"]));
    roots.push(join(drive, &["Program Files", "Steam"]));
    if let Some(home) = std::env::var_os("HOME") {
        let home = Path::new(&home);
        roots.push(join(home, &[".steam", "steam"]));
        roots.push(join(home, &[".local", "share", "Steam"]));
    }
    let mut libraries = vec![];
    for root in roots {
        // steamapps/libraryfolders.vdf lists every library: `"path"  "D:\\SteamLibrary"`
        if let Ok(library_folders) =
            std::fs::read_to_string(join(&root, &["steamapps", "libraryfolders.vdf"]))
        {
            for line in library_folders.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1] == "path" {
                    libraries.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
                }
            }
        }
        libraries.push(root);
    }
    libraries
}

#[cfg(test)]
mod tests {
    #[test]
    fn join_uses_one_separator() {
        let path = super::join(std::path::Path::new("base"), &["first", "second"]);
        let separator = std::path::MAIN_SEPARATOR;
        assert_eq!(
            path.to_string_lossy(),
            format!("base{separator}first{separator}second")
        );
    }
}
