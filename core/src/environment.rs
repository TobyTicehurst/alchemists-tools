//! What the tools need, and whether it's there: the game, UE4SS, the `.usmap` and the mods folders,
//! which players need ([`player_checks`], what the GUI's Setup shows), and the Unreal parts and
//! Oodle, which only making mods needs ([`maker_checks`]). `alchemists-tools check` prints both.

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

const UE4SS_HELP: &str = "What runs Lua and C++ mods in the game. Players need it for any mod \
    with Lua or C++; mods that only change the game's tables or add models don't, as the game \
    loads those itself. The tools install the build mods' C++ is made for, and can disable it \
    after a game update it can't handle yet (docs/examples/lua.md, \"Setting up UE4SS\").";
const MAPPINGS_HELP: &str = "The game's data layouts: its files store values without their \
    names or types, and this tells the tools what they are. Needed to install (or build) any mod \
    that changes the game's tables, since installing merges every mod's table changes; mods of \
    only Lua or C++ don't need it. It must be dumped again after each game update.";

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

/// Every check: the players' and the makers'.
pub fn checks(settings: &Settings) -> Vec<Check> {
    let mut checks = player_checks(settings);
    checks.extend(maker_checks(settings));
    checks
}

/// What playing with mods needs: the game, UE4SS, the mappings and the mods folders.
pub fn player_checks(settings: &Settings) -> Vec<Check> {
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
            help: "Alchemy Factory, installed by Steam: where mods are installed, and whose \
                tables mods change. Its Steam build tells which mods were made for an older one.",
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
        let current = crate::mods::game_build(game);
        let updated = crate::mods::installed_record(game)
            .ok()
            .and_then(|record| record.game_updated(current.as_deref()));
        if let Some((installed, current)) = updated {
            checks.push(Check {
                name: "Game updated",
                status: Status::Warning,
                summary: format!(
                    "build {installed} to {current} since the mods were installed: install them \
                     again"
                ),
                path: None,
                help:
                    "The installed mods' table changes are merged into the game's tables as they \
                    were when the mods were installed, so until they're installed again (Confirm) \
                    the update's own table changes are hidden. Mods' C++ and UE4SS may not work \
                    with the new build yet: if the game crashes at start, disable UE4SS.",
            });
        }
        let binaries = join(&game.root, &["AlchemyFactory", "Binaries", "Win64"]);
        let path = Some(binaries.join("ue4ss"));
        let pinned = crate::ue4ss::VERSION;
        let (status, summary) = match crate::ue4ss::installed(game) {
            crate::ue4ss::Installed::None => (
                Status::Missing,
                format!(
                    "not installed: mods with Lua or C++ can't be installed (install UE4SS \
                     {pinned} here, or: alchemists-tools ue4ss install)"
                ),
            ),
            _ if !crate::ue4ss::has_loader(game) => (
                Status::Warning,
                "UE4SS.dll is there, but not the dwmapi.dll that loads it".to_string(),
            ),
            crate::ue4ss::Installed::Tools { disabled: true, .. }
            | crate::ue4ss::Installed::Other { disabled: true, .. } => (
                Status::Warning,
                "disabled: the game starts without it, so mods' Lua and C++ don't run".to_string(),
            ),
            crate::ue4ss::Installed::Tools { version, .. } => {
                (Status::Found, format!("{version}, installed by the tools"))
            }
            crate::ue4ss::Installed::Other { pinned: true, .. } => {
                (Status::Found, format!("{pinned}, installed by hand"))
            }
            crate::ue4ss::Installed::Other { version, .. } => (
                Status::Warning,
                format!(
                    "{}, installed some other way: mods with C++ made for these tools need \
                     {pinned}",
                    version.unwrap_or_else(|| "a build without a log yet".to_string())
                ),
            ),
        };
        checks.push(Check {
            name: "UE4SS",
            status,
            summary,
            path,
            help: UE4SS_HELP,
        });
        let mod_loader = game.root.join("mods");
        if mod_loader.is_dir() {
            checks.push(Check {
                name: "AF Mod Loader",
                status: Status::Found,
                summary: "installed: its mods (in mods/) are shown, but managed by it".to_string(),
                path: Some(mod_loader),
                help: "Another way to run Lua mods, with its own mods folder. Not needed: \
                    shown so that its mods aren't mistaken for this tool's. Mods installed here \
                    work beside it.",
            });
        }
    }
    let usmap = settings.usmap(game.as_ref());
    let build = game.as_ref().and_then(crate::mods::game_build);
    let shipped = usmap.as_deref().and_then(crate::mappings::shipped_build);
    checks.push(match (&usmap, &game) {
        (Some(path), _) if !path.is_file() => Check {
            name: "Mappings (.usmap)",
            status: Status::Warning,
            summary: format!(
                "{} doesn't exist: mods that change tables can't be installed",
                display_path(path)
            ),
            path: Some(path.clone()),
            help: MAPPINGS_HELP,
        },
        (Some(path), _) if shipped.is_some() => {
            let made_for = shipped.clone().unwrap_or_default();
            match &build {
                Some(build) if *build != made_for => Check {
                    name: "Mappings (.usmap)",
                    status: Status::Warning,
                    summary: format!(
                        "the tools' own are for game build {made_for}, and the game is build \
                         {build}: mods that change tables may not install until a new release \
                         of the tools (or dump new ones with UE4SS: Ctrl + Numpad 6 in a new \
                         save)"
                    ),
                    path: Some(path.clone()),
                    help: MAPPINGS_HELP,
                },
                _ => Check {
                    name: "Mappings (.usmap)",
                    status: Status::Found,
                    summary: format!("the tools' own, for game build {made_for}"),
                    path: Some(path.clone()),
                    help: MAPPINGS_HELP,
                },
            }
        }
        (Some(path), Some(game)) if !crate::update::stale_dumps(game, path).is_empty() => Check {
            name: "Mappings (.usmap)",
            status: Status::Warning,
            summary: "older than the game: dump a new one (Ctrl + Numpad 6 in a new save)"
                .to_string(),
            path: Some(path.clone()),
            help: MAPPINGS_HELP,
        },
        (Some(path), _) => Check {
            name: "Mappings (.usmap)",
            status: Status::Found,
            summary: if settings.usmap.is_some() {
                "as set".to_string()
            } else {
                "dumped by UE4SS".to_string()
            },
            path: Some(path.clone()),
            help: MAPPINGS_HELP,
        },
        (None, _) => Check {
            name: "Mappings (.usmap)",
            status: Status::Warning,
            summary: "none in the game's ue4ss folder: mods that change tables can't be \
                installed (with UE4SS, start a new save and press Ctrl + Numpad 6)"
                .to_string(),
            path: None,
            help: MAPPINGS_HELP,
        },
    });
    if settings.mods.is_empty() {
        checks.push(Check {
            name: "Mods folder",
            status: Status::Missing,
            summary: "none set: the GUI lists no mods, and the command line uses the mods folder \
                where it's started"
                .to_string(),
            path: None,
            help: "The folder your mod folders are in (where unticked mods stay, too). Choose \
                one in Setup, or: alchemists-tools settings --mods <folder>",
        });
    }
    for folder in &settings.mods {
        let count = crate::mods::mod_folders_in(std::slice::from_ref(folder)).len();
        let folder = folder.clone();
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
            help: "Where your mods are kept, installed or not: mod folders, and mods' zips (as \
                downloaded). Unticked mods stay here; the game only sees what's installed.",
        });
    }
    checks
}

/// What making mods needs besides: the stub Unreal project, the engine, and Oodle (which the tools
/// fetch themselves).
pub fn maker_checks(settings: &Settings) -> Vec<Check> {
    let mut checks = vec![];
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
            "not found: only for making mods with new models or Blueprints".to_string()
        },
        path: Some(project),
        help: "For mod makers only: an Unreal project set up by `alchemists-tools stub`, for \
            cooking new models and Blueprints (docs/STUB-PROJECT.md). Players never need it: a \
            mod's zip holds what was cooked.",
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
            "not found: only for making mods with new models or Blueprints".to_string()
        },
        path: Some(engine),
        help: "For mod makers only: from the Epic Games Launcher, to build the stub project. \
            Players never need it.",
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
            help: "The decompressor for the game's packed files, which the tools read the \
                game's tables from. Downloaded by the tools the first time they need it, so \
                there's nothing to install.",
        });
    }
    checks
}
