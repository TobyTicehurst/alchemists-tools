//! What the tools need, and whether it's there: the game, UE4SS, the `.usmap`, the mods folders and
//! the optional Unreal parts. `alchemists-tools check` prints these; the GUI shows them in its setup
//! section.

use std::path::PathBuf;

use crate::game::{display_path, join, Game};
use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// There, and as it should be.
    Found,
    /// There, but something's wrong with it, or it's in the way.
    Warning,
    /// Needed, and not there.
    Missing,
    /// Not there, and only needed for some things.
    Optional,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    /// What was found, or what's wrong.
    pub summary: String,
    pub path: Option<PathBuf>,
    /// What it's for, and how to get it.
    pub help: &'static str,
}

const UE4SS_HELP: &str =
    "Runs Lua and C++ mods, and dumps the .usmap. Install UE4SS's experimental \
    build v3.0.1-1181 into AlchemyFactory/Binaries/Win64/ (docs/examples/lua.md, \"Setting up \
    UE4SS\").";

/// The game's executable, which is running while the game is.
const GAME_PROCESS: &str = "AlchemyFactory-Win64-Shipping.exe";

/// Whether the game is running (which holds its files, so mods can't be installed).
#[cfg(windows)]
pub fn game_running() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {GAME_PROCESS}"), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(GAME_PROCESS))
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn game_running() -> bool {
    false
}

/// The UE4SS version, from the second line of its log (`UE4SS - v3.0.1 Beta #0 - Git SHA #...`).
pub fn ue4ss_version(game: &Game) -> Option<String> {
    let binaries = join(&game.root, &["AlchemyFactory", "Binaries", "Win64"]);
    [binaries.join("ue4ss"), binaries]
        .iter()
        .filter_map(|folder| std::fs::read_to_string(folder.join("UE4SS.log")).ok())
        .find_map(|log| {
            log.lines().take(10).find_map(|line| {
                line.split_once("UE4SS - ")
                    .map(|(_, rest)| rest.to_string())
            })
        })
}

pub fn checks(settings: &Settings) -> Vec<Check> {
    let mut checks = vec![];
    let game = settings.game().filter(|game| game.paks().is_dir());
    checks.push(match &game {
        Some(game) => Check {
            name: "Game",
            status: Status::Found,
            summary: match crate::mods::game_build(game) {
                Some(build) => format!("Steam build {build}"),
                None => "found (no Steam build number: not installed by Steam?)".to_string(),
            },
            path: Some(game.root.clone()),
            help: "Alchemy Factory, installed by Steam.",
        },
        None => Check {
            name: "Game",
            status: Status::Missing,
            summary: match &settings.game {
                Some(path) => format!("{} isn't the game folder", display_path(path)),
                None => "not found in the Steam libraries".to_string(),
            },
            path: settings.game.clone(),
            help: "Alchemy Factory, installed by Steam. Set its folder (the one holding \
                AlchemyFactory/) if it isn't found.",
        },
    });
    if let Some(game) = &game {
        if game_running() {
            checks.push(Check {
                name: "Game running",
                status: Status::Warning,
                summary: "the game is running: close it before installing or uninstalling mods"
                    .to_string(),
                path: None,
                help: "The running game holds its files open.",
            });
        }
        let binaries = join(&game.root, &["AlchemyFactory", "Binaries", "Win64"]);
        let installed = ["ue4ss/UE4SS.dll", "UE4SS.dll"]
            .iter()
            .any(|dll| binaries.join(dll).is_file());
        let proxy = ["dwmapi.dll", "xinput1_3.dll"]
            .iter()
            .any(|dll| binaries.join(dll).is_file());
        checks.push(match (installed, proxy) {
            (true, true) => Check {
                name: "UE4SS",
                status: Status::Found,
                summary: ue4ss_version(game)
                    .map(|version| format!("version {version}"))
                    .unwrap_or_else(|| "installed; no log yet (start the game once)".to_string()),
                path: Some(binaries.join("ue4ss")),
                help: UE4SS_HELP,
            },
            (true, false) => Check {
                name: "UE4SS",
                status: Status::Warning,
                summary: "UE4SS.dll is there, but not the dwmapi.dll that loads it".to_string(),
                path: Some(binaries.join("ue4ss")),
                help: UE4SS_HELP,
            },
            _ => Check {
                name: "UE4SS",
                status: Status::Missing,
                summary: "not installed: Lua and C++ mods can't be installed".to_string(),
                path: None,
                help: UE4SS_HELP,
            },
        });
        let mod_loader = game.root.join("mods");
        if mod_loader.is_dir() {
            checks.push(Check {
                name: "AF Mod Loader",
                status: Status::Found,
                summary: "installed: its mods (in mods/) are shown, but managed by it".to_string(),
                path: Some(mod_loader),
                help: "Another way to run Lua mods; mods installed here work beside it.",
            });
        }
    }
    let usmap = settings.usmap(game.as_ref());
    checks.push(match (&usmap, &game) {
        (Some(path), _) if !path.is_file() => Check {
            name: "Mappings (.usmap)",
            status: Status::Missing,
            summary: format!("{} doesn't exist", display_path(path)),
            path: Some(path.clone()),
            help: "The game's data layouts, which building mods needs.",
        },
        (Some(path), Some(game)) if !crate::update::stale_dumps(game, path).is_empty() => Check {
            name: "Mappings (.usmap)",
            status: Status::Warning,
            summary: "older than the game: dump a new one (Ctrl + Numpad 6 in a new save)"
                .to_string(),
            path: Some(path.clone()),
            help: "The game's data layouts, which building mods needs. Dump one with UE4SS after \
                each game update.",
        },
        (Some(path), _) => Check {
            name: "Mappings (.usmap)",
            status: Status::Found,
            summary: "found".to_string(),
            path: Some(path.clone()),
            help: "The game's data layouts, which building mods needs.",
        },
        (None, _) => Check {
            name: "Mappings (.usmap)",
            status: Status::Missing,
            summary: "none in the game's ue4ss folder: mods can't be built".to_string(),
            path: None,
            help:
                "The game's data layouts, which building mods needs. With UE4SS installed, start \
                a new save and press Ctrl + Numpad 6.",
        },
    });
    for folder in settings.mods() {
        let count = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().join("mod.json").is_file())
            .count();
        checks.push(Check {
            name: "Mods folder",
            status: if !folder.is_dir() {
                Status::Missing
            } else if count == 0 {
                Status::Warning
            } else {
                Status::Found
            },
            summary: if folder.is_dir() {
                format!("{count} mod(s)")
            } else {
                "doesn't exist".to_string()
            },
            path: Some(std::path::absolute(&folder).unwrap_or(folder)),
            help: "A folder of mod folders, each with a mod.json.",
        });
    }
    let project = settings.project();
    checks.push(Check {
        name: "Stub Unreal project",
        status: if project.is_file() {
            Status::Found
        } else {
            Status::Optional
        },
        summary: if project.is_file() {
            "found".to_string()
        } else {
            "not found: only mods with cooked content need it".to_string()
        },
        path: Some(project),
        help: "An Unreal project set up by `alchemists-tools stub`, for cooking meshes and \
            Blueprints (docs/STUB-PROJECT.md).",
    });
    let engine = settings.engine();
    checks.push(Check {
        name: "Unreal Engine 5.7",
        status: if engine.is_dir() {
            Status::Found
        } else {
            Status::Optional
        },
        summary: if engine.is_dir() {
            "found".to_string()
        } else {
            "not found: only setting up the stub project needs it".to_string()
        },
        path: Some(engine),
        help: "From the Epic Games Launcher, for building the stub project.",
    });
    let oodle = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .map(|folder| folder.join("oo2core_9_win64.dll"))
    });
    if let Some(oodle) = oodle {
        let found = oodle.is_file();
        checks.push(Check {
            name: "Oodle",
            status: if found {
                Status::Found
            } else {
                Status::Optional
            },
            summary: if found {
                "found".to_string()
            } else {
                "downloaded the first time the game's files are read".to_string()
            },
            path: Some(oodle),
            help: "The decompressor for the game's files.",
        });
    }
    checks
}
