//! The desktop interface: the mods in the mods folders, each ticked if it's to be installed, and a
//! Confirm button that builds and installs the ticked ones and uninstalls the rest, all at once or
//! not at all (`Workspace::apply`); plus the setup the tools need. Everything here can be done from
//! the command line too: `mod apply`, `mod list`, `check` and `settings`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod work;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use alchemists_core::environment::{Check, Status};
use alchemists_core::game::display_path;
use alchemists_core::mods::{
    parse_version, requirement_text, satisfies, Kind, Origin, Overview, Summary, Unmanaged,
};
use alchemists_core::report::{Level, Message};
use alchemists_core::settings::Settings;
use alchemists_core::ue4ss;
use eframe::egui::{self, Align, Color32, Layout, RichText, Vec2};

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
        };
        app.start(context, Job::Refresh);
        app
    }

    fn start(&mut self, context: &egui::Context, job: Job) {
        if self.running.is_some() {
            return;
        }
        if matches!(job, Job::Apply { .. }) {
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
                self.outcome = Some(Outcome::Succeeded(text));
            }
            Some(Err(error)) => {
                self.outcome = Some(Outcome::Failed(error));
            }
            None => {}
        }
        // after a failure the ticks stay, to fix and try again; otherwise they show what's there
        if !failed {
            self.ticked = self
                .overview
                .installed()
                .map(|summary| summary.name.clone())
                .collect();
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

    fn list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let import = ui
                .add_enabled(
                    self.running.is_none(),
                    egui::Button::new("📥 Import a zip…"),
                )
                .on_hover_text(
                    "Import a mod made without this tool (a download from a mod site), as best \
                     it can be: it's placed safely, or refused, and what can't be checked is noted",
                );
            if import.clicked() {
                let picked = rfd::FileDialog::new()
                    .add_filter("zip", &["zip"])
                    .pick_file();
                if let Some(zip) = picked {
                    let context = ui.ctx().clone();
                    self.start(&context, Job::Import { zip });
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("🔍 Search mods")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(4.0);
        if let Some(error) = &self.overview_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.overview.mods.is_empty() && self.overview.unmanaged.is_empty() {
            if self.running.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the mods…");
                });
            } else {
                ui.label("No mods found.");
                ui.label(
                    RichText::new("Add the folders that hold your mod folders in Setup.").weak(),
                );
                if ui.button("Open Setup").clicked() {
                    self.view = View::Setup;
                }
            }
            return;
        }
        let filter = self.filter.to_lowercase();
        let matches = |name: &str, title: &str| {
            filter.is_empty()
                || name.to_lowercase().contains(&filter)
                || title.to_lowercase().contains(&filter)
        };
        let installed = self.installed_names();
        let current = self.overview.game_build.clone();
        let mut actions = vec![];
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for summary in &self.overview.mods {
                    if !matches(&summary.name, &summary.title) {
                        continue;
                    }
                    let row = Row {
                        summary,
                        ticked: self.ticked.contains(&summary.name),
                        installed: installed.contains(&summary.name),
                        selected: self.selected == Some(Selection::Mod(summary.name.clone())),
                        marks: self
                            .marks
                            .get(&summary.name)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        current: current.as_deref(),
                        enabled: self.running.is_none(),
                    };
                    actions.extend(row.show(ui, &mut self.thumbnails));
                }
                let unmanaged: Vec<&Unmanaged> = self
                    .overview
                    .unmanaged
                    .iter()
                    .filter(|item| matches(&item.name, &item.name))
                    .collect();
                if !unmanaged.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("Installed by hand or by another tool").strong())
                        .on_hover_text(
                            "Files in the game folder that no mod this tool installed accounts \
                             for. It can't install or uninstall them.",
                        );
                    for item in unmanaged {
                        let selected =
                            self.selected == Some(Selection::Unmanaged(item.name.clone()));
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(false, |ui| {
                                ui.add(egui::Checkbox::without_text(&mut true))
                            })
                            .response
                            .on_disabled_hover_text("Not managed by this tool");
                            thumbnail(ui, &mut self.thumbnails, &item.name, &item.name, None, 22.0);
                            let label = ui.selectable_label(selected, &item.name);
                            if label.clicked() {
                                actions
                                    .push(Action::Select(Selection::Unmanaged(item.name.clone())));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let text = if item.mod_loader {
                                    "AF Mod Loader"
                                } else {
                                    "manual"
                                };
                                chip(ui, text, ui.visuals().warn_fg_color)
                                    .on_hover_text(item.describe());
                            });
                        });
                    }
                }
            });
        for action in actions {
            match action {
                Action::Tick(name, tick) => self.tick(&name, tick),
                Action::Select(selection) => self.selected = Some(selection),
            }
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        match self.selected.clone() {
            Some(Selection::Mod(name)) => match self.overview.get(&name).cloned() {
                Some(summary) => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.mod_details(ui, &summary));
                }
                None => {
                    ui.label("That mod is gone.");
                }
            },
            Some(Selection::Unmanaged(name)) => {
                if let Some(item) = self
                    .overview
                    .unmanaged
                    .iter()
                    .find(|item| item.name == name)
                {
                    let item = item.clone();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| unmanaged_details(ui, &item, &mut self.thumbnails));
                }
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Select a mod to see its details").weak());
                });
            }
        }
    }

    fn mod_details(&mut self, ui: &mut egui::Ui, summary: &Summary) {
        let current = self.overview.game_build.clone();
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            thumbnail(
                ui,
                &mut self.thumbnails,
                &summary.name,
                &summary.title,
                summary.thumbnail.as_deref(),
                112.0,
            );
            ui.vertical(|ui| {
                ui.heading(&summary.title);
                let mut line = format!("{} {}", summary.name, summary.version);
                if !summary.author.is_empty() {
                    line += &format!(" · by {}", summary.author);
                }
                ui.label(RichText::new(line).weak());
                ui.add_space(4.0);
                let ticked = self.ticked.contains(&summary.name);
                let state = match (&summary.installed, ticked) {
                    (Some(installed), true) if installed.version != summary.version => {
                        format!(
                            "Installed {}: Confirm updates it to {}",
                            installed.version, summary.version
                        )
                    }
                    (Some(installed), true) => format!("Installed ({})", installed.version),
                    (Some(_), false) => "Installed: Confirm uninstalls it".to_string(),
                    (None, true) => "Not installed: Confirm installs it".to_string(),
                    (None, false) => "Not installed".to_string(),
                };
                ui.label(RichText::new(state).strong());
                ui.horizontal(|ui| {
                    if let Some(folder) = &summary.folder {
                        if ui.button("📁 Open folder").clicked() {
                            open_folder(folder);
                        }
                    }
                });
            });
        });
        ui.add_space(8.0);
        for note in summary.notes(current.as_deref()) {
            callout(ui, "⚠", &note, warn);
        }
        for mark in self.marks.get(&summary.name).into_iter().flatten() {
            let (icon, color) = match mark.level {
                Level::Error => ("✖", error),
                _ => ("⚠", warn),
            };
            callout(ui, icon, &mark.text, color);
        }
        if !summary.description.is_empty() {
            ui.add_space(4.0);
            ui.label(&summary.description);
        }
        ui.add_space(8.0);
        egui::Grid::new("details")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                field(ui, "Name", &summary.name);
                field(ui, "Version", &summary.version);
                ui.label(RichText::new("Game build").weak());
                ui.horizontal_wrapped(|ui| {
                    match &summary.game_build {
                        Some(build) => {
                            let source = if summary.installed.as_ref().is_some_and(|installed| {
                                installed.game_build.as_ref() == Some(build)
                            }) && summary.folder.is_none()
                            {
                                "built against"
                            } else {
                                "made for"
                            };
                            ui.label(format!("{source} {build}"));
                        }
                        None => {
                            ui.label(RichText::new("not given").weak());
                        }
                    }
                    if let Some(current) = &current {
                        if summary.outdated(Some(current)) {
                            ui.colored_label(warn, format!("⚠ the game is build {current}"));
                        } else {
                            ui.label(RichText::new(format!("(the game is {current})")).weak());
                        }
                    }
                });
                ui.end_row();
                if !summary.author.is_empty() {
                    field(ui, "Author", &summary.author);
                }
                if !summary.homepage.is_empty() {
                    ui.label(RichText::new("Homepage").weak());
                    ui.hyperlink(&summary.homepage);
                    ui.end_row();
                }
                if !summary.contact.is_empty() {
                    field(ui, "Contact", &summary.contact);
                }
                if let Some(installed) = &summary.installed {
                    let how = match installed.origin {
                        Origin::Tool => "by this tool",
                        Origin::Zip => "from its zip",
                        Origin::Unverified => "without a manifest",
                        Origin::Imported => "imported by this tool, as best it could",
                    };
                    field(ui, "Installed", &format!("{} ({how})", installed.version));
                }
                if let Some(folder) = &summary.folder {
                    field(ui, "Folder", &display_path(folder));
                }
                if let Some(archive) = &summary.archive {
                    field(ui, "Zip", &display_path(archive));
                }
                ui.label(RichText::new("Dependencies").weak());
                ui.vertical(|ui| {
                    let requires = summary.requires();
                    if requires.is_empty() {
                        ui.label(RichText::new("none").weak());
                    }
                    for dependency in requires {
                        let (icon, color, state) = self.dependency_state(&dependency, ui);
                        ui.horizontal(|ui| {
                            ui.colored_label(color, icon);
                            ui.label(requirement_text(&dependency));
                            ui.label(RichText::new(state).weak().small());
                        });
                    }
                });
                ui.end_row();
                let required_by: Vec<&str> = self
                    .overview
                    .mods
                    .iter()
                    .filter(|other| {
                        other.requires().iter().any(|dependency| {
                            dependency.name == summary.name && dependency.kind.is_required()
                        })
                    })
                    .map(|other| other.name.as_str())
                    .collect();
                if !required_by.is_empty() {
                    field(ui, "Required by", &required_by.join(", "));
                }
                list_field(ui, "Tables it changes", &summary.tables);
                list_field(ui, "Buildings it adds", &summary.new_buildings);
                list_field(ui, "Functions it detours", &summary.detours);
                list_field(ui, "Functions it hooks", &summary.hooks);
            });
    }

    /// An icon, its colour, and a word for whether a dependency is met by the ticked mods.
    fn dependency_state(
        &self,
        dependency: &alchemists_core::mods::Dependency,
        ui: &egui::Ui,
    ) -> (&'static str, Color32, String) {
        let good = Color32::from_rgb(90, 170, 90);
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        let other = self.overview.get(&dependency.name);
        let ticked = self.ticked.contains(&dependency.name);
        let version_ok = other
            .and_then(|other| parse_version(&other.version, &other.name).ok())
            .is_none_or(|version| satisfies(version, dependency));
        match dependency.kind {
            Kind::Incompatible if ticked => ("⊘", error, "incompatible, and ticked".into()),
            Kind::Incompatible => ("⊘", Color32::GRAY, "incompatible".into()),
            kind if kind.is_required() => match (other, ticked) {
                (None, _) => ("✖", error, "required; not found".into()),
                (Some(_), _) if !version_ok => ("✖", error, "required; wrong version".into()),
                (Some(_), true) => ("✔", good, "required".into()),
                (Some(_), false) => ("✖", warn, "required; not ticked".into()),
            },
            Kind::Recommended => (
                if ticked { "✔" } else { "+" },
                if ticked { good } else { Color32::GRAY },
                "recommended".into(),
            ),
            _ => (
                if ticked { "✔" } else { "?" },
                if ticked { good } else { Color32::GRAY },
                "optional".into(),
            ),
        }
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
                 Deconstruct them in every save you keep first.",
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

    fn setup(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("What the tools need");
                    if ui
                        .add_enabled(self.running.is_none(), egui::Button::new("⟳ Check again"))
                        .clicked()
                    {
                        let context = ui.ctx().clone();
                        self.start(&context, Job::Refresh);
                    }
                });
                ui.add_space(6.0);
                self.outcome_banner(ui);
                let mut create: Option<PathBuf> = None;
                let mut ue4ss_action = None;
                let idle = self.running.is_none();
                egui::Grid::new("checks")
                    .num_columns(3)
                    .spacing([12.0, 8.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for check in &self.checks {
                            let (icon, color) = match check.status {
                                Status::Found => ("✔", Color32::from_rgb(90, 170, 90)),
                                Status::Warning => ("⚠", ui.visuals().warn_fg_color),
                                Status::Missing => ("✖", ui.visuals().error_fg_color),
                                Status::Optional => ("–", Color32::GRAY),
                            };
                            ui.colored_label(color, RichText::new(icon).strong());
                            ui.label(RichText::new(check.name).strong())
                                .on_hover_text(check.help);
                            ui.vertical(|ui| {
                                ui.label(&check.summary);
                                if let Some(path) = &check.path {
                                    ui.label(RichText::new(display_path(path)).weak().small());
                                }
                                if matches!(check.status, Status::Missing | Status::Warning) {
                                    ui.add(
                                        egui::Label::new(RichText::new(check.help).small()).wrap(),
                                    );
                                }
                                // a mods folder that's set but not there yet
                                let missing_folder = check.path.as_ref().filter(|_| {
                                    check.name == "Mods folder" && check.status == Status::Missing
                                });
                                if let Some(folder) = missing_folder {
                                    if ui.button("Create it").clicked() {
                                        create = Some(folder.clone());
                                    }
                                }
                                if check.name == "UE4SS" {
                                    ue4ss_action =
                                        ue4ss_action.or(ue4ss_buttons(ui, &self.ue4ss, idle));
                                }
                            });
                            ui.end_row();
                        }
                    });
                if let Some(action) = ue4ss_action {
                    let context = ui.ctx().clone();
                    self.start(&context, Job::Ue4ss(action));
                }
                if let Some(folder) = create {
                    match std::fs::create_dir_all(&folder) {
                        Ok(()) => {
                            let context = ui.ctx().clone();
                            self.start(&context, Job::Refresh);
                        }
                        Err(error) => {
                            self.outcome = Some(Outcome::Failed(format!(
                                "couldn't make {}: {error}",
                                display_path(&folder)
                            )))
                        }
                    }
                }
                ui.add_space(14.0);
                ui.separator();
                ui.heading("Where things are");
                let place = Settings::path()
                    .map(|path| display_path(&path))
                    .unwrap_or_else(|| "nowhere (no APPDATA folder)".to_string());
                ui.label(
                    RichText::new(format!(
                        "Saved in {place}, which the command line reads too. Empty fields use the \
                         default shown."
                    ))
                    .weak(),
                );
                ui.add_space(6.0);
                let editing = &mut self.editing;
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Game folder");
                        path_field(
                            ui,
                            &mut editing.game,
                            "found in the Steam libraries",
                            Pick::Folder,
                        );
                        ui.end_row();
                        ui.label(".usmap mappings");
                        path_field(
                            ui,
                            &mut editing.usmap,
                            "the newest in the game's ue4ss folder",
                            Pick::File("usmap"),
                        );
                        ui.end_row();
                        ui.label("Mods folders");
                        ui.vertical(|ui| {
                            let mut remove = None;
                            for (index, folder) in editing.mods.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    let mut value = Some(folder.clone());
                                    path_field(ui, &mut value, "", Pick::Folder);
                                    match value {
                                        Some(value) => *folder = value,
                                        None => remove = Some(index),
                                    }
                                    if ui.button("Remove").clicked() {
                                        remove = Some(index);
                                    }
                                });
                            }
                            if let Some(index) = remove {
                                editing.mods.remove(index);
                            }
                            if editing.mods.is_empty() {
                                ui.label(RichText::new("none set").weak());
                            }
                            if ui
                                .button("➕ Add a mods folder…")
                                .on_hover_text(
                                    "A folder of mod folders, each with a mod.json. A mod in \
                                     more than one folder is taken from the first.",
                                )
                                .clicked()
                            {
                                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                    editing.mods.push(folder);
                                }
                            }
                        });
                        ui.end_row();
                    });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let changed = self.editing != self.settings;
                    if ui
                        .add_enabled(changed && self.running.is_none(), egui::Button::new("Save"))
                        .clicked()
                    {
                        match self.editing.save() {
                            Ok(()) => {
                                self.settings = self.editing.clone();
                                let context = ui.ctx().clone();
                                self.start(&context, Job::Refresh);
                            }
                            Err(error) => {
                                self.outcome = Some(Outcome::Failed(format!(
                                    "couldn't save the settings: {error:#}"
                                )))
                            }
                        }
                    }
                    if ui
                        .add_enabled(changed, egui::Button::new("Discard"))
                        .clicked()
                    {
                        self.editing = self.settings.clone();
                    }
                });
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive();
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        match self.view {
            View::Mods => {
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

/// One mod in the list.
struct Row<'a> {
    summary: &'a Summary,
    ticked: bool,
    installed: bool,
    selected: bool,
    /// The last apply's warnings and errors about it.
    marks: &'a [Message],
    current: Option<&'a str>,
    enabled: bool,
}

impl Row<'_> {
    fn show(&self, ui: &mut egui::Ui, thumbnails: &mut Thumbnails) -> Vec<Action> {
        let summary = self.summary;
        let mut actions = vec![];
        ui.horizontal(|ui| {
            let mut ticked = self.ticked;
            // a mod that can't be built can still be unticked, to uninstall it
            let can_tick = self.enabled && (summary.installable() || self.installed);
            let checkbox = ui
                .add_enabled(can_tick, egui::Checkbox::without_text(&mut ticked))
                .on_hover_text(if ticked {
                    "Installed once confirmed"
                } else {
                    "Not installed once confirmed"
                });
            if checkbox.changed() {
                actions.push(Action::Tick(summary.name.clone(), ticked));
            }
            thumbnail(
                ui,
                thumbnails,
                &summary.name,
                &summary.title,
                summary.thumbnail.as_deref(),
                22.0,
            );
            let mut title = RichText::new(&summary.title);
            if self.selected {
                title = title.strong();
            }
            let label = ui.selectable_label(self.selected, title);
            if label.clicked() || checkbox.clicked() {
                actions.push(Action::Select(Selection::Mod(summary.name.clone())));
            }
            ui.label(RichText::new(&summary.version).weak().small());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let warn = ui.visuals().warn_fg_color;
                let error = ui.visuals().error_fg_color;
                // right to left: the last apply's marks first
                let errors: Vec<&str> = self
                    .marks
                    .iter()
                    .filter(|mark| mark.level == Level::Error)
                    .map(|mark| mark.text.as_str())
                    .collect();
                let warnings: Vec<&str> = self
                    .marks
                    .iter()
                    .filter(|mark| mark.level == Level::Warning)
                    .map(|mark| mark.text.as_str())
                    .collect();
                if !errors.is_empty() {
                    ui.colored_label(error, RichText::new("✖").strong())
                        .on_hover_text(errors.join("\n\n"));
                }
                if !warnings.is_empty() {
                    ui.colored_label(warn, RichText::new("⚠").strong())
                        .on_hover_text(warnings.join("\n\n"));
                }
                if summary.error.is_some() {
                    chip(ui, "broken", error)
                        .on_hover_text(summary.error.clone().unwrap_or_default());
                }
                if summary.outdated(self.current) {
                    chip(ui, "old game build", warn).on_hover_text(format!(
                        "Made for game build {}; the game is build {}. It may need updating.",
                        summary.game_build.as_deref().unwrap_or_default(),
                        self.current.unwrap_or_default()
                    ));
                }
                if let Some(installed) = &summary.installed {
                    match installed.origin {
                        Origin::Tool => {}
                        Origin::Zip => {
                            chip(ui, "from zip", warn).on_hover_text(
                                "Installed from its zip: its table edits aren't merged with the \
                                 other mods'. Install it from its mod folder to merge them.",
                            );
                        }
                        Origin::Unverified => {
                            chip(ui, "no manifest", warn).on_hover_text(
                                "Installed without a manifest (by mods.py or an older version), \
                                 so its files can't be checked. Confirm reinstalls it with one; \
                                 uninstalling it needs Force.",
                            );
                        }
                        Origin::Imported => {}
                    }
                    if !installed.changed.is_empty() || !installed.missing.is_empty() {
                        chip(ui, "files changed", warn).on_hover_text(
                            installed
                                .changed
                                .iter()
                                .chain(&installed.missing)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                }
                if summary.folder.is_none() {
                    if summary.archive.is_some() {
                        chip(ui, "zip", Color32::GRAY).on_hover_text(
                            "A zip in the mods folder: it can be installed and \
                             uninstalled, not rebuilt",
                        );
                    } else if self.installed {
                        chip(ui, "no source", Color32::GRAY).on_hover_text(
                            "Installed, but not in the mods folders: unticking it keeps a copy \
                             there as a zip, to install it again",
                        );
                    }
                }
                // an imported mod: what couldn't be checked about it
                if !summary.cautions.is_empty() {
                    chip(ui, "imported", warn).on_hover_text(summary.cautions.join("\n\n"));
                }
                let accent = ui.visuals().selection.bg_fill;
                match (self.installed, self.ticked) {
                    (false, true) => {
                        chip(ui, "install", accent);
                    }
                    (true, false) => {
                        chip(ui, "uninstall", accent);
                    }
                    (true, true)
                        if summary.installed.as_ref().is_some_and(|installed| {
                            summary.folder.is_some() && installed.version != summary.version
                        }) =>
                    {
                        chip(ui, "update", accent);
                    }
                    _ => {}
                }
            });
        });
        actions
    }
}

fn unmanaged_details(ui: &mut egui::Ui, item: &Unmanaged, thumbnails: &mut Thumbnails) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        thumbnail(ui, thumbnails, &item.name, &item.name, None, 112.0);
        ui.vertical(|ui| {
            ui.heading(&item.name);
            ui.label(RichText::new(item.describe()).weak());
        });
    });
    ui.add_space(8.0);
    let text = if item.mod_loader {
        "Managed by AF Mod Loader, in the game's mods folder. This tool leaves it alone; mods \
         installed here work beside it."
    } else {
        "Installed by hand or by another tool: no manifest lists these files, so this tool can't \
         install or uninstall them. If its containers replace a table that the ticked mods edit, \
         only one of them takes effect. To remove it, delete its files (alchemists-tools mod \
         uninstall <name> --force deletes files named for a mod)."
    };
    callout(ui, "⚠", text, ui.visuals().warn_fg_color);
    if !item.replaces.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Game assets it replaces").strong());
        for asset in &item.replaces {
            ui.monospace(asset);
        }
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Files").strong());
    for file in &item.files {
        ui.monospace(file);
    }
}

/// The buttons for UE4SS, by what's installed: install it; disable or enable it; remove the one
/// the tools installed. Returns what was clicked.
fn ue4ss_buttons(
    ui: &mut egui::Ui,
    installed: &Option<ue4ss::Installed>,
    idle: bool,
) -> Option<work::Ue4ssAction> {
    use work::Ue4ssAction;
    let installed = installed.as_ref()?;
    let mut clicked = None;
    ui.horizontal(|ui| {
        let mut button = |ui: &mut egui::Ui, text: &str, hint: &str, action: Ue4ssAction| {
            if ui
                .add_enabled(idle, egui::Button::new(text))
                .on_hover_text(hint)
                .clicked()
            {
                clicked = Some(action);
            }
        };
        let disabled = match installed {
            ue4ss::Installed::None => {
                button(
                    ui,
                    &format!("Install UE4SS {}", ue4ss::VERSION),
                    "Download the build mods' C++ is made for (checked against its checksum) and \
                     install it into the game",
                    Ue4ssAction::Install,
                );
                return;
            }
            ue4ss::Installed::Tools { disabled, .. } => *disabled,
            ue4ss::Installed::Other {
                pinned, disabled, ..
            } => {
                if *pinned {
                    button(
                        ui,
                        "Let the tools manage it",
                        "It's the build the tools install: they take it over as it is, so they \
                         can remove it later",
                        Ue4ssAction::Install,
                    );
                }
                *disabled
            }
        };
        if disabled {
            button(
                ui,
                "Enable",
                "Start the game with UE4SS again",
                Ue4ssAction::Enable,
            );
        } else {
            button(
                ui,
                "Disable",
                "Start the game without UE4SS, as after a game update it can't handle yet; mods' \
                 Lua and C++ don't run",
                Ue4ssAction::Disable,
            );
        }
        if matches!(installed, ue4ss::Installed::Tools { .. }) {
            button(
                ui,
                "Remove",
                "Remove the UE4SS the tools installed: only its files, and only those unchanged",
                Ue4ssAction::Remove,
            );
        }
    });
    clicked
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

fn field(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(RichText::new(name).weak());
    ui.add(egui::Label::new(value).wrap());
    ui.end_row();
}

fn list_field(ui: &mut egui::Ui, name: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    ui.label(RichText::new(name).weak());
    ui.vertical(|ui| {
        for value in values {
            ui.label(RichText::new(value).monospace().small());
        }
    });
    ui.end_row();
}

/// A highlighted line: an icon and some text in a colour.
fn callout(ui: &mut egui::Ui, icon: &str, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .corner_radius(4)
        .inner_margin(6)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.colored_label(color, RichText::new(icon).strong());
                ui.add(egui::Label::new(text).wrap());
            });
        });
    ui.add_space(3.0);
}

/// A small coloured tag.
fn chip(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(4, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)))
        .response
}

enum Pick {
    Folder,
    /// A file with this extension.
    File(&'static str),
}

/// A path that may be left to its default: typed, or picked with a dialog.
fn path_field(ui: &mut egui::Ui, value: &mut Option<PathBuf>, default: &str, pick: Pick) {
    ui.horizontal(|ui| {
        let mut text = value
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let edit = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(default)
                .desired_width(460.0),
        );
        if edit.changed() {
            *value = (!text.is_empty()).then(|| PathBuf::from(&text));
        }
        if ui.button("Browse…").clicked() {
            let dialog = rfd::FileDialog::new();
            let picked = match pick {
                Pick::Folder => dialog.pick_folder(),
                Pick::File(extension) => dialog.add_filter(extension, &[extension]).pick_file(),
            };
            if picked.is_some() {
                *value = picked;
            }
        }
    });
}

fn open_folder(folder: &Path) {
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(windows))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(folder).spawn();
}

/// Mods' pictures, loaded once each.
#[derive(Default)]
struct Thumbnails {
    loaded: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl Thumbnails {
    fn get(&mut self, context: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        self.loaded
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                let picture = image::open(path).ok()?.thumbnail(256, 256).to_rgba8();
                let size = [picture.width() as usize, picture.height() as usize];
                Some(context.load_texture(
                    path.display().to_string(),
                    egui::ColorImage::from_rgba_unmultiplied(size, &picture),
                    egui::TextureOptions::LINEAR,
                ))
            })
            .clone()
    }
}

/// A mod's picture, or else its initials on a colour of its own.
fn thumbnail(
    ui: &mut egui::Ui,
    thumbnails: &mut Thumbnails,
    name: &str,
    title: &str,
    path: Option<&Path>,
    size: f32,
) {
    let texture = path.and_then(|path| thumbnails.get(ui.ctx(), path));
    let corner = (size * 0.12) as u8;
    match texture {
        Some(texture) => {
            ui.add(
                egui::Image::new(&texture)
                    .fit_to_exact_size(Vec2::splat(size))
                    .corner_radius(corner),
            );
        }
        None => {
            let (rectangle, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
            // the same colour for a name every time
            let hash = name.bytes().fold(2166136261u32, |hash, byte| {
                (hash ^ byte as u32).wrapping_mul(16777619)
            });
            let hue = (hash % 360) as f32 / 360.0;
            let fill = Color32::from(egui::ecolor::Hsva::new(hue, 0.45, 0.6, 1.0));
            ui.painter().rect_filled(rectangle, corner, fill);
            // each word's first letter or digit ("Alchemist's Vault" -> AV)
            let initials: String = title
                .split_whitespace()
                .filter_map(|word| word.chars().find(|character| character.is_alphanumeric()))
                .take(2)
                .collect::<String>()
                .to_uppercase();
            let initials = if initials.chars().count() < 2 {
                // a one-word name in CamelCase: its capitals
                let capitals: String = title
                    .chars()
                    .filter(|character| character.is_uppercase())
                    .take(2)
                    .collect();
                if capitals.chars().count() == 2 {
                    capitals
                } else {
                    initials
                }
            } else {
                initials
            };
            ui.painter().text(
                rectangle.center(),
                egui::Align2::CENTER_CENTER,
                initials,
                egui::FontId::proportional(size * 0.42),
                Color32::WHITE,
            );
        }
    }
}
