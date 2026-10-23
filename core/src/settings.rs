//! Where things are, saved for the GUI and the command line alike: in
//! `%APPDATA%\alchemists-tools\settings.json` (`~/.config/alchemists-tools/` elsewhere). Anything
//! not set is found (the game, its `.usmap`) or has a default; the command line's options override
//! them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::game::{display_path, Game};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Settings {
    /// The game folder (the one holding `AlchemyFactory/`) [default: found in the Steam libraries].
    pub game: Option<PathBuf>,
    /// The `.usmap` mappings [default: the newest in the game's `ue4ss` folder].
    pub usmap: Option<PathBuf>,
    /// The folders of mod folders; the first gets new mods [default: `mods`].
    pub mods: Vec<PathBuf>,
    /// Where builds go [default: `build`].
    pub build: Option<PathBuf>,
    /// The stub Unreal project, for mods with cooked content.
    pub project: Option<PathBuf>,
    /// The Unreal Engine install, for building the stub project.
    pub engine: Option<PathBuf>,
}

/// `Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject`.
pub fn default_project() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").unwrap_or_default();
    PathBuf::from(home).join("Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject")
}

pub const DEFAULT_ENGINE: &str = "C:/Program Files/Epic Games/UE_5.7";

impl Settings {
    /// The folder settings are kept in.
    pub fn folder() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
        Some(base.join("alchemists-tools"))
    }

    pub fn path() -> Option<PathBuf> {
        Self::folder().map(|folder| folder.join("settings.json"))
    }

    /// The saved settings, or the defaults if none are saved.
    pub fn load() -> Result<Self> {
        let Some(path) = Self::path().filter(|path| path.exists()) else {
            return Ok(Settings::default());
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", display_path(&path)))?;
        // as some editors save it, with a byte order mark
        let text = text.trim_start_matches('\u{feff}');
        serde_json::from_str(text).with_context(|| format!("{} isn't valid", display_path(&path)))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("no APPDATA or HOME folder to keep settings in")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", display_path(&path)))
    }

    /// The game: as set, or found in the Steam libraries.
    pub fn game(&self) -> Option<Game> {
        match &self.game {
            Some(path) => Some(Game::new(path)),
            None => Game::find(),
        }
    }

    /// The `.usmap`: as set; else the tools' own for the game's build (see [`crate::mappings`]);
    /// else the newest the game's UE4SS dumped; else the tools' newest, for another build.
    pub fn usmap(&self, game: Option<&Game>) -> Option<PathBuf> {
        if let Some(path) = &self.usmap {
            return Some(path.clone());
        }
        game.and_then(crate::mods::game_build)
            .and_then(|build| crate::mappings::for_build(&build))
            .or_else(|| game.and_then(Game::find_usmap))
            .or_else(|| crate::mappings::newest().map(|(_, path)| path))
    }

    pub fn mods(&self) -> Vec<PathBuf> {
        if self.mods.is_empty() {
            vec![PathBuf::from("mods")]
        } else {
            self.mods.clone()
        }
    }

    pub fn build(&self) -> PathBuf {
        self.build.clone().unwrap_or_else(|| PathBuf::from("build"))
    }

    pub fn project(&self) -> PathBuf {
        self.project.clone().unwrap_or_else(default_project)
    }

    pub fn engine(&self) -> PathBuf {
        self.engine
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ENGINE))
    }
}
