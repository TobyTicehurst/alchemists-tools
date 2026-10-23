//! The Setup view: what the tools need, and where things are.

use super::*;

impl App {
    pub fn setup(&mut self, ui: &mut egui::Ui) {
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
