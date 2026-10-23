//! The work the GUI does in the background, so the window keeps drawing: reading the mods and
//! checking the setup, and applying the ticked mods. Each job reports its messages as it goes.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use alchemists_core::environment::{self, Check};
use alchemists_core::game::Game;
use alchemists_core::mods::{Applied, Overview, Workspace};
use alchemists_core::report::{self, Message};
use alchemists_core::settings::Settings;
use eframe::egui;

pub enum Job {
    /// Read the mods and check the setup.
    Refresh,
    /// Make the installed mods exactly these (see `Workspace::apply`).
    Apply { wanted: Vec<String>, force: bool },
    /// Clone a git repository of mods into the mods folder.
    AddFromGit { url: String, branch: Option<String> },
    /// Pull the repository a mod was added from.
    UpdateFromGit { name: String },
}

/// What a job found: always the mods and the setup as they are afterwards.
pub struct Done {
    pub overview: Result<Overview, String>,
    pub checks: Vec<Check>,
    /// For an apply: what it did, or why it failed.
    pub applied: Option<Result<Applied, String>>,
    /// For the other jobs that change something: what was done, or why it failed.
    pub action: Option<Result<String, String>>,
}

pub enum Event {
    Message(Message),
    Done(Box<Done>),
}

/// A job running on its own thread.
pub struct Running {
    pub applying: bool,
    receiver: Receiver<Event>,
}

impl Running {
    pub fn start(job: Job, settings: Settings, context: egui::Context) -> Self {
        let applying = matches!(job, Job::Apply { .. });
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
        Running { applying, receiver }
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
    let (mut applied, mut action) = (None, None);
    match job {
        Job::Refresh => {}
        Job::Apply { wanted, force } => {
            applied = Some(if !workspace.game.paks().is_dir() {
                Err("the game wasn't found: set its folder in Setup".to_string())
            } else {
                workspace.apply(&wanted, force).map_err(text)
            })
        }
        Job::AddFromGit { url, branch } => {
            action = Some(
                workspace
                    .add_from_git(&url, branch.as_deref())
                    .map(|names| {
                        format!(
                            "Added {} from git: tick to install, then Confirm.",
                            names.join(", ")
                        )
                    })
                    .map_err(text),
            )
        }
        Job::UpdateFromGit { name } => {
            action = Some(
                workspace
                    .update_from_git(&name)
                    .map(|_| format!("Updated {name} from git: Confirm installs the new version."))
                    .map_err(text),
            )
        }
    }
    Done {
        overview: workspace.overview().map_err(text),
        checks: environment::checks(settings),
        applied,
        action,
    }
}
