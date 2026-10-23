//! The Mods view: the list of mods, and the selected one's details.

use super::*;

impl App {
    /// Pick a zip, and import it.
    pub fn import_zip(&mut self, context: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .add_filter("zip", &["zip"])
            .pick_file();
        if let Some(zip) = picked {
            self.start(context, Job::Import { zip });
        }
    }

    pub fn list(&mut self, ui: &mut egui::Ui) {
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
                let context = ui.ctx().clone();
                self.import_zip(&context);
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
        let ue4ss_missing = matches!(self.ue4ss, Some(ue4ss::Installed::None));
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
                        ue4ss_missing,
                        nexus_update: self
                            .browse
                            .newer_version(summary)
                            .map(|found| found.version.as_str()),
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

    pub fn details(&mut self, ui: &mut egui::Ui) {
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
        if let Some(found) = self.browse.newer_version(summary) {
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().hyperlink_color, "⬇");
                ui.label(format!(
                    "Nexus Mods has version {}, updated {}:",
                    found.version, found.updated
                ));
                ui.hyperlink_to("download it from its page", found.page());
            });
        }
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
    /// The game was found without UE4SS.
    ue4ss_missing: bool,
    /// A newer version on Nexus Mods.
    nexus_update: Option<&'a str>,
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
            if checkbox.clicked() {
                actions.push(Action::Select(Selection::Mod(summary.name.clone())));
            }
            // the tags first, from the right; then the title, shortened to fit what's left
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
                if let Some(version) = self.nexus_update {
                    chip(ui, "update on Nexus", ui.visuals().hyperlink_color).on_hover_text(
                        format!(
                            "Nexus Mods has version {version}: download it from the mod's page \
                             (its details link it)"
                        ),
                    );
                }
                // what it needs and changes, known before it's ticked
                if summary.changes_tables {
                    chip(ui, "changes tables", Color32::GRAY).on_hover_text(
                        "Changes the game's tables (recipes, items, research...): merged with the \
                         other mods' changes when it's installed",
                    );
                }
                if summary.needs_ue4ss {
                    let (color, hint) = if self.ue4ss_missing {
                        (
                            warn,
                            "Has Lua or C++ that UE4SS runs, and UE4SS isn't installed: install \
                             it in Setup first",
                        )
                    } else {
                        (Color32::GRAY, "Has Lua or C++ that UE4SS runs")
                    };
                    chip(ui, "needs UE4SS", color).on_hover_text(hint);
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
                ui.label(RichText::new(&summary.version).weak().small());
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let mut title = RichText::new(&summary.title);
                    if self.selected {
                        title = title.strong();
                    }
                    let label = ui
                        .add(egui::Button::selectable(self.selected, title).truncate())
                        .on_hover_text(&summary.title);
                    if label.clicked() {
                        actions.push(Action::Select(Selection::Mod(summary.name.clone())));
                    }
                });
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
