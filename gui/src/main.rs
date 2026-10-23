//! The desktop interface: the mods in the mods folders, each ticked if it's to be installed, and a
//! Confirm button that builds and installs the ticked ones and uninstalls the rest, all at once or
//! not at all (`Workspace::apply`); plus the setup the tools need. Everything here can be done from
//! the command line too: `mod apply`, `mod list`, `check` and `settings`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browse;
mod list;
mod setup;
mod widgets;
mod work;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use alchemists_core::environment::{Check, Status};
use alchemists_core::game::display_path;
use alchemists_core::mods::{
    parse_version, requirement_text, satisfies, Kind, Origin, Overview, Summary, Unmanaged,
};
use alchemists_core::nexus;
use alchemists_core::release::{self, Release};
use alchemists_core::report::{Level, Message};
use alchemists_core::settings::Settings;
use alchemists_core::ue4ss;
use eframe::egui::{self, Align, Color32, Layout, RichText, Vec2};

use widgets::*;
use work::{Event, Job, Running};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Alchemist's Tools")
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Alchemist's Tools",
        options,
        Box::new(|creation| Ok(Box::new(App::new(&creation.egui_ctx)))),
    )
}

#[derive(Clone, PartialEq)]
enum Selection {
    Mod(String),
    Unmanaged(String),
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Mods,
    Nexus,
    Setup,
}

/// How the last apply went.
enum Outcome {
    Succeeded(String),
    Failed(String),
}

/// What clicking in the list asked for, done once the list is drawn.
enum Action {
    Tick(String, bool),
    Select(Selection),
}

struct App {
    settings: Settings,
    /// The settings as edited in Setup, until saved.
    editing: Settings,
    overview: Overview,
    overview_error: Option<String>,
    checks: Vec<Check>,
    /// UE4SS in the game folder, if the game was found.
    ue4ss: Option<ue4ss::Installed>,
    /// The mods to have installed once Confirm is clicked.
    ticked: BTreeSet<String>,
    selected: Option<Selection>,
    filter: String,
    force: bool,
    view: View,
    running: Option<Running>,
    /// Every message of the last job.
    log: Vec<Message>,
    show_log: bool,
    /// The last apply's warnings and errors, by the mod they're about.
    marks: BTreeMap<String, Vec<Message>>,
    outcome: Option<Outcome>,
    /// What ticking a mod also did (ticked what it requires, say).
    hint: Option<String>,
    /// Asking before uninstalling mods whose buildings crash saves.
    confirming: bool,
    thumbnails: Thumbnails,
    /// A newer release of the tools, once the check (on its own thread) has said.
    newer_release: Option<Release>,
    release_check: Option<mpsc::Receiver<Option<Release>>>,
    browse: browse::Browse,
}

/// How much larger than egui's own the text is.
const TEXT_SCALE: f32 = 1.15;

impl App {
    fn new(context: &egui::Context) -> Self {
        context.all_styles_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size *= TEXT_SCALE;
            }
        });
        let (settings, outcome) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(Outcome::Failed(format!(
                    "couldn't read the settings: {error:#}"
                ))),
            ),
        };
        let mut app = App {
            editing: settings.clone(),
            settings,
            overview: Overview::default(),
            overview_error: None,
            checks: vec![],
            ue4ss: None,
            ticked: BTreeSet::new(),
            selected: None,
            filter: String::new(),
            force: false,
            view: View::Mods,
            running: None,
            log: vec![],
            show_log: false,
            marks: BTreeMap::new(),
            outcome,
            hint: None,
            confirming: false,
            thumbnails: Thumbnails::default(),
            newer_release: None,
            release_check: None,
            browse: browse::Browse::new(),
        };
        app.start(context, Job::Refresh);
        let (sender, receiver) = mpsc::channel();
        let repaint = context.clone();
        std::thread::spawn(move || {
            // a failed check (offline, say) shows nothing
            let _ = sender.send(release::newer_release().ok().flatten());
            repaint.request_repaint();
        });
        app.release_check = Some(receiver);
        app
    }

    fn start(&mut self, context: &egui::Context, job: Job) {
        if self.running.is_some() {
            return;
        }
        if matches!(job, Job::Apply { .. } | Job::Check { .. }) {
            self.marks.clear();
            self.outcome = None;
        }
        self.log.clear();
        self.hint = None;
        self.thumbnails.loaded.clear();
        self.running = Some(Running::start(job, self.settings.clone(), context.clone()));
    }

    fn apply(&mut self, context: &egui::Context) {
        let job = Job::Apply {
            wanted: self.ticked.iter().cloned().collect(),
            force: self.force,
        };
        self.start(context, job);
    }

    /// Take in what the running job has sent.
    fn receive(&mut self) {
        let Some(running) = &self.running else {
            return;
        };
        let applying = running.applying;
        let (events, stopped) = running.events();
        for event in events {
            match event {
                Event::Message(message) => {
                    if applying && message.level != Level::Note {
                        for name in &message.mods {
                            let marks = self.marks.entry(name.clone()).or_default();
                            if !marks.iter().any(|mark| mark.text == message.text) {
                                marks.push(message.clone());
                            }
                        }
                    }
                    self.log.push(message);
                }
                Event::Done(done) => {
                    self.running = None;
                    self.finish(*done);
                }
            }
        }
        if stopped {
            self.running = None;
            self.show_log = true;
            self.outcome = Some(Outcome::Failed(
                "the work stopped unexpectedly; the log shows how far it got".to_string(),
            ));
        }
    }

    fn finish(&mut self, done: work::Done) {
        match done.overview {
            Ok(overview) => {
                self.overview = overview;
                self.overview_error = None;
            }
            Err(error) => self.overview_error = Some(error),
        }
        self.checks = done.checks;
        self.ue4ss = done.ue4ss;
        match done.action {
            Some(Ok(text)) => self.outcome = Some(Outcome::Succeeded(text)),
            Some(Err(error)) => self.outcome = Some(Outcome::Failed(error)),
            None => {}
        }
        let failed = matches!(done.applied, Some(Err(_)));
        match done.applied {
            Some(Ok(applied)) => {
                let warnings = self
                    .log
                    .iter()
                    .filter(|message| message.level == Level::Warning)
                    .count();
                let mut text = format!(
                    "Done: {} mod(s) installed{}. Restart the game to load them.",
                    applied.installed.len(),
                    if applied.uninstalled.is_empty() {
                        String::new()
                    } else {
                        format!(", {} uninstalled", applied.uninstalled.len())
                    }
                );
                if warnings > 0 {
                    text += &format!(" {warnings} warning(s): the marked mods, and the log.");
                }
                if let Some(backup) = &applied.saves_backup {
                    text += &format!(
                        " Your saves were backed up first, to {}: to go back, copy its folders \
                         into the game's SaveGames folder.",
                        display_path(backup)
                    );
                }
                self.outcome = Some(Outcome::Succeeded(text));
            }
            Some(Err(error)) => {
                self.outcome = Some(Outcome::Failed(error));
            }
            None => {}
        }
        // a check changes nothing, so the ticks stay
        let checking = done.checked.is_some();
        match done.checked {
            Some(Ok(planned)) => {
                let mut parts = vec![];
                if !planned.installed.is_empty() {
                    parts.push(format!("install {}", planned.installed.join(", ")));
                }
                if !planned.uninstalled.is_empty() {
                    parts.push(format!("uninstall {}", planned.uninstalled.join(", ")));
                }
                let warnings = self
                    .log
                    .iter()
                    .filter(|message| message.level == Level::Warning)
                    .count();
                let mut text = format!(
                    "Checked: nothing stops Confirm. It would {}. Nothing was changed.",
                    if parts.is_empty() {
                        "change nothing".to_string()
                    } else {
                        parts.join(", and ")
                    }
                );
                if warnings > 0 {
                    text += &format!(" {warnings} warning(s): the marked mods, and the log.");
                }
                self.outcome = Some(Outcome::Succeeded(text));
            }
            Some(Err(error)) => self.outcome = Some(Outcome::Failed(error)),
            None => {}
        }
        // after a failure the ticks stay, to fix and try again; otherwise they show what's there
        if !failed && !checking {
            self.ticked = self
                .overview
                .installed()
                .map(|summary| summary.name.clone())
                .collect();
        }
        // a list from another player: tick its mods, and say what differs
        match done.compared {
            Some(Ok(comparison)) => {
                self.ticked = comparison.wanted.into_iter().collect();
                let text = if comparison.differences.is_empty() {
                    "The installed mods match the list.".to_string()
                } else {
                    format!(
                        "The list's mods that are here are ticked: Confirm installs exactly \
                         those.\n• {}",
                        comparison.differences.join("\n• ")
                    )
                };
                self.outcome = Some(Outcome::Succeeded(text));
            }
            Some(Err(error)) => self.outcome = Some(Outcome::Failed(error)),
            None => {}
        }
        let exists = match &self.selected {
            Some(Selection::Mod(name)) => self.overview.get(name).is_some(),
            Some(Selection::Unmanaged(name)) => self
                .overview
                .unmanaged
                .iter()
                .any(|item| item.name == *name),
            None => true,
        };
        if !exists {
            self.selected = None;
        }
    }

    fn installed_names(&self) -> BTreeSet<String> {
        self.overview
            .installed()
            .map(|summary| summary.name.clone())
            .collect()
    }

    fn game_found(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.name == "Game" && check.status == Status::Found)
    }

    fn tick(&mut self, name: &str, tick: bool) {
        let before = self.ticked.clone();
        if tick {
            self.overview.tick(&mut self.ticked, name);
        } else {
            self.overview.untick(&mut self.ticked, name);
        }
        let others: Vec<String> = self
            .ticked
            .symmetric_difference(&before)
            .filter(|other| *other != name)
            .cloned()
            .collect();
        self.hint = (!others.is_empty()).then(|| {
            if tick {
                format!("Also ticked {}, which {name} requires.", others.join(", "))
            } else {
                format!("Also unticked {}, which require {name}.", others.join(", "))
            }
        });
    }

    // --- drawing ---

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Alchemist's Tools");
            ui.separator();
            ui.selectable_value(&mut self.view, View::Mods, "Mods");
            ui.selectable_value(&mut self.view, View::Nexus, "Nexus Mods");
            let problems = self
                .checks
                .iter()
                .filter(|check| matches!(check.status, Status::Missing | Status::Warning))
                .count();
            let setup = if problems > 0 {
                RichText::new(format!("⚠ Setup ({problems})")).color(ui.visuals().warn_fg_color)
            } else {
                RichText::new("Setup")
            };
            ui.selectable_value(&mut self.view, View::Setup, setup);
            if let Some(release) = &self.newer_release {
                ui.separator();
                let text = RichText::new(format!("Version {} is out", release.version))
                    .color(ui.visuals().hyperlink_color);
                ui.hyperlink_to(text, &release.page).on_hover_text(format!(
                    "A newer release of Alchemist's Tools (this is {}): download its installer \
                     from this page and run it to update",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let reload = ui
                    .add_enabled(self.running.is_none(), egui::Button::new("⟳ Reload"))
                    .on_hover_text("Read the mods folders and the game folder again (pending changes are dropped)");
                if reload.clicked() {
                    let context = ui.ctx().clone();
                    self.start(&context, Job::Refresh);
                }
                ui.separator();
                if let Some(build) = &self.overview.game_build {
                    ui.label(RichText::new(format!("game build {build}")).weak());
                    ui.separator();
                }
                let installed = self.overview.installed().count();
                let mut text = format!(
                    "{installed} of {} mods installed",
                    self.overview.mods.len()
                );
                if !self.overview.unmanaged.is_empty() {
                    text += &format!(", {} other", self.overview.unmanaged.len());
                }
                ui.label(RichText::new(text).strong());
            });
        });
    }

    /// How the last job went, until closed.
    fn outcome_banner(&mut self, ui: &mut egui::Ui) {
        if let Some(outcome) = &self.outcome {
            let (icon, text, color) = match outcome {
                Outcome::Succeeded(text) => ("✔", text, Color32::from_rgb(90, 170, 90)),
                Outcome::Failed(text) => ("✖", text, ui.visuals().error_fg_color),
            };
            let mut close = false;
            egui::Frame::new()
                .fill(color.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.7)))
                .corner_radius(4)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, RichText::new(icon).heading());
                        egui::ScrollArea::vertical()
                            .max_height(110.0)
                            .id_salt("outcome")
                            .show(ui, |ui| {
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    close = ui.small_button("✖").on_hover_text("Close").clicked();
                                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                        ui.add(egui::Label::new(text.as_str()).wrap());
                                    });
                                });
                            });
                    });
                });
            if close {
                self.outcome = None;
            }
            ui.add_space(4.0);
        }
    }

    /// The game has updated since the mods were installed: what that means, and what to do.
    fn game_updated_notice(&mut self, ui: &mut egui::Ui) {
        let Some((installed, current)) = self.overview.game_updated.clone() else {
            return;
        };
        let warn = ui.visuals().warn_fg_color;
        let ue4ss_enabled = matches!(
            self.ue4ss,
            Some(
                ue4ss::Installed::Tools {
                    disabled: false,
                    ..
                } | ue4ss::Installed::Other {
                    disabled: false,
                    ..
                }
            )
        );
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(warn.gamma_multiply(0.10))
            .corner_radius(4)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.colored_label(warn, RichText::new("⚠").heading());
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "The game has updated (build {installed} to {current}) since the \
                                 mods were installed"
                            ))
                            .strong(),
                        );
                        ui.add(
                            egui::Label::new(
                                "Confirm installs them again, merging their table changes into \
                                 the new game's tables: until then, they hide the update's own \
                                 changes. If the game now crashes at start, UE4SS or a mod's C++ \
                                 isn't ready for the new build: disable UE4SS until they're \
                                 updated.",
                            )
                            .wrap(),
                        );
                        if ue4ss_enabled {
                            let disable = ui
                                .add_enabled(
                                    self.running.is_none(),
                                    egui::Button::new("Disable UE4SS"),
                                )
                                .on_hover_text(
                                    "Start the game without UE4SS (mods' Lua and C++ don't run); \
                                     enable it again in Setup",
                                );
                            if disable.clicked() {
                                let context = ui.ctx().clone();
                                self.start(&context, Job::Ue4ss(work::Ue4ssAction::Disable));
                            }
                        }
                    });
                });
            });
        ui.add_space(4.0);
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        self.outcome_banner(ui);
        let installed = self.installed_names();
        let installing: Vec<String> = self.ticked.difference(&installed).cloned().collect();
        let removing: Vec<String> = installed.difference(&self.ticked).cloned().collect();
        ui.horizontal(|ui| {
            if let Some(running) = &self.running {
                ui.spinner();
                let last = self
                    .log
                    .iter()
                    .rev()
                    .find(|message| !message.text.trim().is_empty())
                    .map(|message| message.text.trim().to_string())
                    .unwrap_or_default();
                ui.label(RichText::new(running.doing).strong());
                ui.add(egui::Label::new(RichText::new(last).weak()).truncate());
            } else {
                let mut parts = vec![];
                if !installing.is_empty() {
                    parts.push(format!("install {}", installing.join(", ")));
                }
                if !removing.is_empty() {
                    parts.push(format!("uninstall {}", removing.join(", ")));
                }
                let text = if parts.is_empty() {
                    "No changes ticked: Confirm rebuilds and reinstalls the ticked mods".to_string()
                } else {
                    format!("Confirm will {}", parts.join("; "))
                };
                ui.add(egui::Label::new(text).truncate());
            }
        });
        if let Some(hint) = &self.hint {
            ui.label(RichText::new(hint).weak().small());
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let log = if self.show_log {
                "Hide log"
            } else {
                "Show log"
            };
            if ui.button(log).clicked() {
                self.show_log = !self.show_log;
            }
            let idle = self.running.is_none();
            let save = ui
                .add_enabled(idle, egui::Button::new("Save mod list…"))
                .on_hover_text(
                    "Save the installed mods (names, versions, checksums) as a list for the \
                     players you play with: everyone in a multiplayer game needs the same mods",
                );
            if save.clicked() {
                let picked = rfd::FileDialog::new()
                    .add_filter("mod list", &["json"])
                    .set_file_name("mod-list.json")
                    .save_file();
                if let Some(path) = picked {
                    let context = ui.ctx().clone();
                    self.start(&context, Job::ExportList { path });
                }
            }
            let load = ui
                .add_enabled(idle, egui::Button::new("Load mod list…"))
                .on_hover_text(
                    "Compare another player's mod list with your mods: its mods are ticked, and \
                     what differs is said",
                );
            if load.clicked() {
                let picked = rfd::FileDialog::new()
                    .add_filter("mod list", &["json"])
                    .pick_file();
                if let Some(path) = picked {
                    let context = ui.ctx().clone();
                    self.start(&context, Job::CompareList { path });
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let idle = self.running.is_none();
                let game = self.game_found();
                let confirm = ui
                    .add_enabled(
                        idle && game,
                        egui::Button::new(RichText::new("✔ Confirm").strong())
                            .min_size(Vec2::new(110.0, 28.0)),
                    )
                    .on_hover_text(
                        "Build the ticked mods and install them, and uninstall the others: all \
                         at once, or, if anything fails, not at all",
                    )
                    .on_disabled_hover_text(if game {
                        "Working…"
                    } else {
                        "The game wasn't found: set its folder in Setup"
                    });
                if confirm.clicked() {
                    let buildings = removing.iter().any(|name| {
                        self.overview
                            .get(name)
                            .is_some_and(|summary| !summary.new_buildings.is_empty())
                    });
                    if buildings {
                        self.confirming = true;
                    } else {
                        let context = ui.ctx().clone();
                        self.apply(&context);
                    }
                }
                let check = ui
                    .add_enabled(
                        idle && game,
                        egui::Button::new("Check").min_size(Vec2::new(70.0, 28.0)),
                    )
                    .on_hover_text(
                        "Build the ticked mods and check them together, as Confirm does, \
                         without changing anything: problems show before you commit",
                    );
                if check.clicked() {
                    let job = Job::Check {
                        wanted: self.ticked.iter().cloned().collect(),
                        force: self.force,
                    };
                    let context = ui.ctx().clone();
                    self.start(&context, job);
                }
                let changed = !installing.is_empty() || !removing.is_empty();
                if ui
                    .add_enabled(idle && changed, egui::Button::new("Revert"))
                    .on_hover_text("Tick what's installed again")
                    .clicked()
                {
                    self.ticked = installed.clone();
                    self.hint = None;
                }
                ui.add_enabled(idle, egui::Checkbox::new(&mut self.force, "Force"))
                    .on_hover_text(
                        "Also delete files that changed since their mod installed them, and the \
                         files of mods installed without a manifest; replace merged files that \
                         changed",
                    );
            });
        });
        ui.add_space(4.0);
    }

    fn confirm_dialog(&mut self, context: &egui::Context) {
        let installed = self.installed_names();
        let buildings: Vec<String> = installed
            .difference(&self.ticked)
            .filter_map(|name| self.overview.get(name))
            .flat_map(|summary| summary.new_buildings.iter().cloned())
            .collect();
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("buildings")).show(context, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Uninstall mods that add buildings?");
            ui.add_space(6.0);
            ui.label(format!(
                "A save with a placed {} crashes the game once its mod is uninstalled. \
                 Deconstruct them in every save you keep first. (Your saves are backed up \
                 before anything changes, and reinstalling the mod makes them load again.)",
                buildings.join(", ")
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Uninstall them").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if let Some(go) = answer {
            self.confirming = false;
            if go {
                self.apply(context);
            }
        }
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            if ui.small_button("Copy").clicked() {
                let text: Vec<String> = self.log.iter().map(log_line).collect();
                ui.ctx().copy_text(text.join("\n"));
            }
        });
        egui::ScrollArea::both()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for message in &self.log {
                    let text = RichText::new(log_line(message)).monospace();
                    let text = match message.level {
                        Level::Note => text,
                        Level::Warning => text.color(ui.visuals().warn_fg_color),
                        Level::Error => text.color(ui.visuals().error_fg_color),
                    };
                    ui.label(text);
                }
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive();
        self.browse.receive();
        // mods linked to their Nexus page are marked when Nexus has a newer version
        let linked = self
            .overview
            .mods
            .iter()
            .any(|summary| nexus::mod_id_in(&summary.homepage).is_some());
        if linked {
            self.browse.load_once(ui.ctx());
        }
        if let Some(check) = &self.release_check {
            if let Ok(release) = check.try_recv() {
                self.newer_release = release;
                self.release_check = None;
            }
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        match self.view {
            View::Mods => {
                if self.overview.game_updated.is_some() {
                    egui::Panel::top("game updated").show(ui, |ui| self.game_updated_notice(ui));
                }
                egui::Panel::bottom("actions").show(ui, |ui| self.actions(ui));
                if self.show_log {
                    egui::Panel::bottom("log")
                        .resizable(true)
                        .default_size(200.0)
                        .show(ui, |ui| self.log_view(ui));
                }
                egui::Panel::left("list")
                    .resizable(true)
                    .default_size(400.0)
                    .min_size(300.0)
                    .show(ui, |ui| self.list(ui));
                egui::CentralPanel::default().show(ui, |ui| self.details(ui));
            }
            View::Nexus => {
                let idle = self.running.is_none();
                let asked = self
                    .browse
                    .show(ui, &self.overview, &mut self.thumbnails, idle);
                match asked {
                    Some(browse::Asked::ShowMod(name)) => {
                        self.selected = Some(Selection::Mod(name));
                        self.view = View::Mods;
                    }
                    Some(browse::Asked::Import) => {
                        let context = ui.ctx().clone();
                        self.import_zip(&context);
                        self.view = View::Mods;
                    }
                    None => {}
                }
            }
            View::Setup => {
                egui::CentralPanel::default().show(ui, |ui| self.setup(ui));
            }
        }
        if self.confirming {
            let context = ui.ctx().clone();
            self.confirm_dialog(&context);
        }
    }
}

fn log_line(message: &Message) -> String {
    let about = if message.mods.is_empty() || message.level == Level::Note {
        String::new()
    } else {
        format!("[{}] ", message.mods.join(", "))
    };
    match message.level {
        Level::Note => message.text.clone(),
        Level::Warning => format!("warning: {about}{}", message.text),
        Level::Error => format!("error: {about}{}", message.text),
    }
}
