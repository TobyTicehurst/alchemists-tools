//! The game's mappings (`.usmap`: the layouts of its data, dumped from the running game by UE4SS),
//! shipped with the tools so that players needn't install UE4SS and dump their own. They're files
//! beside the tools, not in them: `mappings/<Steam build>.usmap`, which the installer puts next to
//! the tools (and which, run from a build in the repository, are the repository's `mappings/`).
//! After a game update, a new release brings the new build's (CONTRIBUTING.md, "Releasing").
//!
//! Which mappings are used (see [`crate::settings::Settings::usmap`]): those set in the settings;
//! else the tools' own for the game's build; else the player's own dump; else the tools' newest,
//! made for another build, which may not read the game's tables (reading stops at the first value
//! that doesn't fit, rather than reading it wrongly).

use std::path::{Path, PathBuf};

/// The tools' mappings folder: `mappings/` beside them, or in a folder above them (a build in the
/// repository, `target/release/`).
fn folder() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(4)
        .map(|folder| folder.join("mappings"))
        .find(|folder| folder.is_dir())
}

/// The tools' mappings: (Steam build, file), oldest build first.
fn shipped() -> Vec<(u64, PathBuf)> {
    let Some(folder) = folder() else {
        return vec![];
    };
    let mut found: Vec<(u64, PathBuf)> = std::fs::read_dir(&folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("usmap"))
        })
        .filter_map(|path| {
            let build = path.file_stem()?.to_str()?.parse().ok()?;
            Some((build, path))
        })
        .collect();
    found.sort();
    found
}

/// The tools' mappings for the game's build `build`, if they have them.
pub fn for_build(build: &str) -> Option<PathBuf> {
    let build: u64 = build.parse().ok()?;
    shipped()
        .into_iter()
        .find(|(shipped, _)| *shipped == build)
        .map(|(_, path)| path)
}

/// The tools' newest mappings, and the build they're for.
pub fn newest() -> Option<(String, PathBuf)> {
    shipped()
        .pop()
        .map(|(build, path)| (build.to_string(), path))
}

/// The build of the mappings at `path`, if they're the tools' own.
pub fn shipped_build(path: &Path) -> Option<String> {
    shipped()
        .into_iter()
        .find(|(_, shipped)| shipped == path)
        .map(|(build, _)| build.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn shipped_mappings_read() {
        // run from the repository's target/, so its mappings/ are found
        let (build, path) = super::newest().unwrap();
        assert_eq!(super::shipped_build(&path), Some(build.clone()));
        assert_eq!(super::for_build(&build), Some(path.clone()));
        let schemas = crate::usmap::read_usmap(&path).unwrap();
        assert!(schemas.enums.contains_key("EBeltTDCraftType"));
    }
}
