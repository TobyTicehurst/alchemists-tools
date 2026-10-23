//! The Nexus Mods view: the game's mods on Nexus, read in the background (with their pictures,
//! kept in `nexus-pictures/` beside the settings, and the selected mod's files), each marked if
//! it's in the mods folders already, with its page to download it from (`mod browse`).

use std::sync::mpsc;

use alchemists_core::nexus::{self, NexusFile, NexusMod};

use super::*;

/// What came back from the background.
enum Loaded {
    Mods(Result<Vec<NexusMod>, String>),
    Picture(u32, PathBuf),
    Files(u32, Result<Vec<NexusFile>, String>),
}

/// What the view asks the app to do.
pub enum Asked {
    /// Show this mod in the Mods view.
    ShowMod(String),
    /// Pick a downloaded zip and import it.
    Import,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Endorsements,
    Downloads,
    Updated,
    Name,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Endorsements => "Most endorsed",
            Sort::Downloads => "Most downloaded",
            Sort::Updated => "Recently updated",
            Sort::Name => "Name",
        }
    }
}

pub struct Browse {
    mods: Vec<NexusMod>,
    error: Option<String>,
    /// Whether the mods are being read.
    loading: bool,
    started: bool,
    sender: mpsc::Sender<Loaded>,
    receiver: mpsc::Receiver<Loaded>,
    pictures: HashMap<u32, PathBuf>,
    files: HashMap<u32, Result<Vec<NexusFile>, String>>,
    selected: Option<u32>,
    filter: String,
    sort: Sort,
}

impl Browse {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Browse {
            mods: vec![],
            error: None,
            loading: false,
            started: false,
            sender,
            receiver,
            pictures: HashMap::new(),
            files: HashMap::new(),
            selected: None,
            filter: String::new(),
            sort: Sort::Endorsements,
        }
    }

    /// Read the mods (again), then fetch the pictures not kept already.
    fn load(&mut self, context: &egui::Context) {
        self.loading = true;
        self.started = true;
        self.error = None;
        let sender = self.sender.clone();
        let context = context.clone();
        std::thread::spawn(move || {
            let found = nexus::mods();
            let wanted: Vec<(u32, String)> = match &found {
                Ok(mods) => mods
                    .iter()
                    .filter_map(|found| Some((found.id, found.thumbnail.clone()?)))
                    .collect(),
                Err(_) => vec![],
            };
            let _ = sender.send(Loaded::Mods(found.map_err(|error| format!("{error:#}"))));
            context.request_repaint();
            let Some(folder) = Settings::folder().map(|folder| folder.join("nexus-pictures"))
            else {
                return;
            };
            let _ = std::fs::create_dir_all(&folder);
            for (id, url) in wanted {
                // named as on Nexus, which names each upload anew
                let Some(name) = url.rsplit('/').next().filter(|name| !name.is_empty()) else {
                    continue;
                };
                let path = folder.join(name);
                if !path.exists() {
                    match alchemists_core::web::get(&url, &[], 4 << 20) {
                        Ok(bytes) => {
                            if std::fs::write(&path, bytes).is_err() {
                                continue;
                            }
                        }
                        Err(_) => continue,
                    }
                }
                let _ = sender.send(Loaded::Picture(id, path));
                context.request_repaint();
            }
        });
    }

    fn load_files(&mut self, context: &egui::Context, id: u32) {
        if self.files.contains_key(&id) {
            return;
        }
        self.files.insert(id, Ok(vec![]));
        let sender = self.sender.clone();
        let context = context.clone();
        std::thread::spawn(move || {
            let files = nexus::files(id).map_err(|error| format!("{error:#}"));
            let _ = sender.send(Loaded::Files(id, files));
            context.request_repaint();
        });
    }

    fn receive(&mut self) {
        while let Ok(loaded) = self.receiver.try_recv() {
            match loaded {
                Loaded::Mods(Ok(mods)) => {
                    self.mods = mods;
                    self.loading = false;
                }
                Loaded::Mods(Err(error)) => {
                    self.error = Some(error);
                    self.loading = false;
                }
                Loaded::Picture(id, path) => {
                    self.pictures.insert(id, path);
                }
                Loaded::Files(id, files) => {
                    self.files.insert(id, files);
                }
            }
        }
    }

    /// The view, in the window's root `ui`; returns what it asked for.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        overview: &Overview,
        thumbnails: &mut Thumbnails,
        idle: bool,
    ) -> Option<Asked> {
        self.receive();
        if !self.started {
            let context = ui.ctx().clone();
            self.load(&context);
        }
        let mut asked = None;
        egui::Panel::left("nexus list")
            .resizable(true)
            .default_size(460.0)
            .min_size(320.0)
            .show(ui, |ui| self.list(ui, overview, thumbnails));
        egui::CentralPanel::default().show(ui, |ui| {
            asked = self.details(ui, overview, thumbnails, idle);
        });
        asked
    }

    fn list(&mut self, ui: &mut egui::Ui, overview: &Overview, thumbnails: &mut Thumbnails) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let reload = ui
                .add_enabled(!self.loading, egui::Button::new("⟳"))
                .on_hover_text("Read the mods on Nexus Mods again");
            if reload.clicked() {
                let context = ui.ctx().clone();
                self.load(&context);
            }
            egui::ComboBox::from_id_salt("nexus sort")
                .selected_text(self.sort.label())
                .show_ui(ui, |ui| {
                    for sort in [
                        Sort::Endorsements,
                        Sort::Downloads,
                        Sort::Updated,
                        Sort::Name,
                    ] {
                        ui.selectable_value(&mut self.sort, sort, sort.label());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("🔍 Search Nexus Mods")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(4.0);
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Couldn't read the mods on Nexus Mods: {error}"),
            );
            if ui.button("Try again").clicked() {
                let context = ui.ctx().clone();
                self.load(&context);
            }
            return;
        }
        if self.loading && self.mods.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the mods on Nexus Mods…");
            });
            return;
        }
        let filter = self.filter.to_lowercase();
        let mut shown: Vec<&NexusMod> = self
            .mods
            .iter()
            .filter(|found| {
                filter.is_empty()
                    || found.name.to_lowercase().contains(&filter)
                    || found.summary.to_lowercase().contains(&filter)
                    || found.author.to_lowercase().contains(&filter)
            })
            .collect();
        match self.sort {
            Sort::Endorsements => shown.sort_by_key(|found| std::cmp::Reverse(found.endorsements)),
            Sort::Downloads => shown.sort_by_key(|found| std::cmp::Reverse(found.downloads)),
            Sort::Updated => shown.sort_by(|a, b| b.updated.cmp(&a.updated)),
            Sort::Name => shown.sort_by_key(|found| found.name.to_lowercase()),
        }
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for found in shown {
                    let selected = self.selected == Some(found.id);
                    ui.horizontal(|ui| {
                        picture(
                            ui,
                            thumbnails,
                            self.pictures.get(&found.id),
                            &found.name,
                            64.0,
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // clear of the scroll bar
                            ui.add_space(8.0);
                            if let Some(here) = overview.from_nexus(found) {
                                let accent = Color32::from_rgb(90, 170, 90);
                                let text = if here.installed.is_some() {
                                    "installed"
                                } else {
                                    "in your mods"
                                };
                                chip(ui, text, accent)
                                    .on_hover_text(format!("In the Mods view as {}", here.title));
                            }
                            ui.label(
                                RichText::new(format!("👍 {}", found.endorsements))
                                    .weak()
                                    .small(),
                            )
                            .on_hover_text(format!(
                                "{} endorsements, {} downloads",
                                found.endorsements, found.downloads
                            ));
                            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                let mut title = RichText::new(&found.name);
                                if selected {
                                    title = title.strong();
                                }
                                let label = ui
                                    .add(egui::Button::selectable(selected, title).truncate())
                                    .on_hover_text(&found.summary);
                                if label.clicked() {
                                    clicked = Some(found.id);
                                }
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "by {} · {}",
                                            found.author, found.version
                                        ))
                                        .weak()
                                        .small(),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    });
                    ui.add_space(2.0);
                }
            });
        if let Some(id) = clicked {
            self.selected = Some(id);
            let context = ui.ctx().clone();
            self.load_files(&context, id);
        }
    }

    fn details(
        &mut self,
        ui: &mut egui::Ui,
        overview: &Overview,
        thumbnails: &mut Thumbnails,
        idle: bool,
    ) -> Option<Asked> {
        let Some(found) = self
            .selected
            .and_then(|id| self.mods.iter().find(|found| found.id == id))
        else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "The mods for Alchemy Factory on Nexus Mods: select one to see its details",
                    )
                    .weak(),
                );
            });
            return None;
        };
        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    picture(
                        ui,
                        thumbnails,
                        self.pictures.get(&found.id),
                        &found.name,
                        240.0,
                    );
                    ui.vertical(|ui| {
                        ui.heading(&found.name);
                        ui.label(
                            RichText::new(format!(
                                "Version {} · by {}",
                                found.version, found.author
                            ))
                            .weak(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "👍 {} endorsements · {} downloads",
                                found.endorsements, found.downloads
                            ))
                            .weak(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Published {}, updated {}",
                                found.created, found.updated
                            ))
                            .weak(),
                        );
                        ui.add_space(4.0);
                        if ui.button("🌐 Open on Nexus Mods").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(found.page()));
                        }
                    });
                });
                ui.add_space(8.0);
                ui.add(egui::Label::new(&found.summary).wrap());
                ui.add_space(8.0);
                match overview.from_nexus(found) {
                    Some(here) => {
                        let text = match &here.installed {
                            Some(installed) => format!(
                                "Installed, as {} {} (Nexus has {}).",
                                here.title, installed.version, found.version
                            ),
                            None => format!(
                                "In your mods folders, as {} {} (Nexus has {}): tick it in Mods \
                                 to install it.",
                                here.title, here.version, found.version
                            ),
                        };
                        callout(ui, "✔", &text, Color32::from_rgb(90, 170, 90));
                        if ui.button("Show it in Mods").clicked() {
                            asked = Some(Asked::ShowMod(here.name.clone()));
                        }
                    }
                    None => {
                        ui.label(RichText::new("To install it").strong());
                        ui.add(
                            egui::Label::new(
                                "Open it on Nexus Mods, and download its main file from Files \
                                 (Manual download). Then import the zip: it's placed safely, or \
                                 refused, and what can't be checked is noted. (A zip made with \
                                 these tools can go straight into a mods folder instead.)",
                            )
                            .wrap(),
                        );
                        ui.add_space(4.0);
                        let import = ui
                            .add_enabled(idle, egui::Button::new("📥 Import a zip…"))
                            .on_disabled_hover_text("Working…");
                        if import.clicked() {
                            asked = Some(Asked::Import);
                        }
                    }
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Files").strong());
                match self.files.get(&found.id) {
                    Some(Ok(files)) if files.is_empty() => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Reading its files…");
                        });
                    }
                    Some(Ok(files)) => {
                        let current: Vec<&NexusFile> =
                            files.iter().filter(|file| file.is_current()).collect();
                        if current.is_empty() {
                            ui.label(RichText::new("none current").weak());
                        }
                        egui::Grid::new("nexus files")
                            .num_columns(4)
                            .spacing([16.0, 6.0])
                            .striped(true)
                            .show(ui, |ui| {
                                for file in current {
                                    ui.label(&file.name).on_hover_text(&file.description);
                                    ui.label(RichText::new(category(&file.category)).weak());
                                    ui.label(RichText::new(size(file.size)).weak());
                                    ui.label(RichText::new(&file.uploaded).weak());
                                    ui.end_row();
                                }
                            });
                        let old =
                            files.len() - files.iter().filter(|file| file.is_current()).count();
                        if old > 0 {
                            ui.label(
                                RichText::new(format!("and {old} older or archived file(s)"))
                                    .weak()
                                    .small(),
                            );
                        }
                    }
                    Some(Err(error)) => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            format!("Couldn't read its files: {error}"),
                        );
                    }
                    None => {}
                }
            });
        asked
    }
}

/// A Nexus mod's picture, as wide as `width` (in proportion), or its initials while it loads.
fn picture(
    ui: &mut egui::Ui,
    thumbnails: &mut Thumbnails,
    path: Option<&PathBuf>,
    name: &str,
    width: f32,
) {
    let size = Vec2::new(width, width * 9.0 / 16.0);
    match path.and_then(|path| thumbnails.get(ui.ctx(), path)) {
        Some(texture) => {
            let (rectangle, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            egui::Image::new(&texture)
                .corner_radius((width * 0.05) as u8)
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(
                        rectangle.center(),
                        texture.size_vec2() * (size / texture.size_vec2()).min_elem(),
                    ),
                );
        }
        None => {
            let (rectangle, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rectangle, (width * 0.05) as u8, ui.visuals().faint_bg_color);
            let initial: String = name.chars().take(1).collect();
            ui.painter().text(
                rectangle.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                egui::FontId::proportional(size.y * 0.5),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

fn category(category: &str) -> &str {
    match category {
        "MAIN" => "main",
        "UPDATE" => "update",
        "OPTIONAL" => "optional",
        "MISCELLANEOUS" => "other",
        other => other,
    }
}

/// `8.2 KB`, `1.4 MB`.
fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}
