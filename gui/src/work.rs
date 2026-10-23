//! The work the GUI does in the background, so the window keeps drawing: reading the mods and
//! checking the setup, and applying the ticked mods. Each job reports its messages as it goes.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use alchemists_core::environment::{self, Check};
use alchemists_core::game::{display_path, Game};
use alchemists_core::mods::{Applied, Comparison, ModList, Overview, Workspace};
use alchemists_core::report::{self, Message};
use alchemists_core::settings::Settings;
use alchemists_core::ue4ss;
use eframe::egui;

pub enum Job {
    /// Read the mods and check the setup.
    Refresh,
    /// Make the installed mods exactly these (see `Workspace::apply`).
    Apply { wanted: Vec<String>, force: bool },
    /// Import a mod made without this tool (see `alchemists_core::import`).
    Import { zip: PathBuf },
    /// Install, remove, disable or enable UE4SS (see `alchemists_core::ue4ss`).
    Ue4ss(Ue4ssAction),
    /// Write the installed mods as a list for other players (see `alchemists_core::mods::ModList`).
    ExportList { path: PathBuf },
    /// Compare a list from another player with the mods here.
    CompareList { path: PathBuf },
}

#[derive(Clone, Copy)]
pub enum Ue4ssAction {
    Install,
    Remove,
    Disable,
    Enable,
}

/// What a job found: always the mods and the setup as they are afterwards.
pub struct Done {
    pub overview: Result<Overview, String>,
    pub checks: Vec<Check>,
    /// For an apply: what it did, or why it failed.
    pub applied: Option<Result<Applied, String>>,
    /// For the other jobs that change something: what was done, or why it failed.
    pub action: Option<Result<String, String>>,
    /// For comparing a list: what differs, or why it couldn't be compared.
    pub compared: Option<Result<Comparison, String>>,
    /// UE4SS in the game folder, if the game was found.
    pub ue4ss: Option<ue4ss::Installed>,
}

pub enum Event {
    Message(Message),
    Done(Box<Done>),
}

/// A job running on its own thread.
pub struct Running {
    pub applying: bool,
    /// What it's doing, for the window to say.
    pub doing: &'static str,
    receiver: Receiver<Event>,
}

impl Running {
    pub fn start(job: Job, settings: Settings, context: egui::Context) -> Self {
        let applying = matches!(job, Job::Apply { .. });
        let doing = match &job {
            Job::Refresh => "Reading the mods…",
            Job::Apply { .. } => "Building and installing…",
            Job::Import { .. } => "Importing…",
            Job::ExportList { .. } => "Saving the mod list…",
            Job::CompareList { .. } => "Comparing the mod list…",
            Job::Ue4ss(Ue4ssAction::Install) => "Installing UE4SS…",
            Job::Ue4ss(_) => "Changing UE4SS…",
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let messages = sender.clone();
            let repaint = context.clone();
            let done = report::collect(
                move |message| {
                    let _ = messages.send(Event::Message(message));
                    repaint.request_repaint();
                },
                || run(job, &settings),
            );
            let _ = sender.send(Event::Done(Box::new(done)));
            context.request_repaint();
        });
        Running {
            applying,
            doing,
            receiver,
        }
    }

    /// The events since last asked, and whether the job stopped without finishing (it panicked).
    pub fn events(&self) -> (Vec<Event>, bool) {
        let mut events = vec![];
        loop {
            match self.receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => return (events, false),
                Err(TryRecvError::Disconnected) => {
                    let finished = events.iter().any(|event| matches!(event, Event::Done(_)));
                    return (events, !finished);
                }
            }
        }
    }
}

/// Where builds go: as set, or else beside the GUI's other files (not in whichever folder it was
/// started from).
pub fn build_folder(settings: &Settings) -> PathBuf {
    settings.build.clone().unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(|folder| PathBuf::from(folder).join("alchemists-tools").join("build"))
            .unwrap_or_else(|| settings.build())
    })
}

fn workspace(settings: &Settings) -> Workspace {
    // without a game, the mods folders can still be listed
    let game = settings.game().unwrap_or_else(|| Game::new(PathBuf::new()));
    let usmap = settings.usmap(Some(&game)).unwrap_or_default();
    // only the mods folders set: not one relative to wherever the GUI was started
    Workspace::new(
        settings.mods.clone(),
        build_folder(settings),
        game,
        usmap,
        settings.project(),
    )
}

fn run(job: Job, settings: &Settings) -> Done {
    let workspace = workspace(settings);
    let text = |error: anyhow::Error| format!("{error:#}");
    let (mut applied, mut action, mut compared) = (None, None, None);
    match job {
        Job::Refresh => {}
        Job::Apply { wanted, force } => {
            applied = Some(if !workspace.game.paks().is_dir() {
                Err("the game wasn't found: set its folder in Setup".to_string())
            } else {
                workspace.apply(&wanted, force).map_err(text)
            })
        }
        Job::Ue4ss(wanted) => {
            let game = &workspace.game;
            let done = if !game.paks().is_dir() {
                Err(anyhow::anyhow!(
                    "the game wasn't found: set its folder in Setup"
                ))
            } else {
                match wanted {
                    Ue4ssAction::Install => ue4ss::install(game, None)
                        .map(|_| format!("UE4SS {} is installed.", ue4ss::VERSION)),
                    Ue4ssAction::Remove => {
                        ue4ss::remove(game, false).map(|_| "UE4SS is removed.".to_string())
                    }
                    Ue4ssAction::Disable => ue4ss::set_enabled(game, false)
                        .map(|_| "UE4SS is disabled: the game starts without it.".to_string()),
                    Ue4ssAction::Enable => ue4ss::set_enabled(game, true)
                        .map(|_| "UE4SS is enabled again.".to_string()),
                }
            };
            action = Some(done.map_err(text));
        }
        Job::ExportList { path } => {
            action = Some(
                workspace
                    .export_list(&path)
                    .map(|list| {
                        format!(
                            "Saved the list of the {} installed mods to {}. Send it to the players \
                             you play with: they load it to see what differs.",
                            list.mods.len(),
                            display_path(&path)
                        )
                    })
                    .map_err(text),
            );
        }
        Job::CompareList { path } => {
            compared = Some(
                ModList::read(&path)
                    .and_then(|list| workspace.compare_list(&list))
                    .map_err(text),
            );
        }
        Job::Import { zip } => {
            action = Some(
                workspace
                    .import(&zip, None, None)
                    .map(|imported| {
                        format!(
                            "Imported {} as best it could be: tick to install, then Confirm. \
                             Read its cautions first (in its details).",
                            imported.name
                        )
                    })
                    .map_err(text),
            )
        }
    }
    Done {
        overview: workspace.overview().map_err(text),
        checks: environment::player_checks(settings),
        applied,
        action,
        compared,
        ue4ss: workspace
            .game
            .paks()
            .is_dir()
            .then(|| ue4ss::installed(&workspace.game)),
    }
}
