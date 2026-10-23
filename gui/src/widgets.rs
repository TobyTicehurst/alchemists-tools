//! Small pieces the views share: fields, callouts, chips, and mods' pictures.

use super::*;

pub fn field(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(RichText::new(name).weak());
    ui.add(egui::Label::new(value).wrap());
    ui.end_row();
}

pub fn list_field(ui: &mut egui::Ui, name: &str, values: &[String]) {
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
pub fn callout(ui: &mut egui::Ui, icon: &str, text: &str, color: Color32) {
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
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(4, 1))
        .show(ui, |ui| ui.label(RichText::new(text).small().color(color)))
        .response
}

pub fn open_folder(folder: &Path) {
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(windows))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(folder).spawn();
}

/// Mods' pictures, loaded once each.
#[derive(Default)]
pub struct Thumbnails {
    pub loaded: HashMap<PathBuf, Option<egui::TextureHandle>>,
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
pub fn thumbnail(
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
