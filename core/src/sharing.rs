//! Sharing a mod: `mod dist` makes one zip that is at once a Thunderstore package, a download for
//! Nexus Mods (or any site), and an install for players with or without this tool:
//!
//! - at its top, what Thunderstore reads: `manifest.json`, `icon.png` (256x256), `README.md`, and
//!   `CHANGELOG.md` if the mod folder has one;
//! - `AlchemyFactory/...`: the built files as they go in the game folder, for copying in by hand:
//!   the mod's own data container and template save among them, and its file manifest
//!   (`Paks/~mods/manifests/<name>.json`);
//! - `alchemists-tools-entry.json`: the mod as built, its table and template-save edits among it,
//!   so this tool, finding the zip in a mods folder, installs it merged with the other mods instead
//!   of with its own data container.
//!
//! `manifest.json` holds Thunderstore's fields, and this tool's under a key of its own,
//! `alchemists-tools`, so that neither clashes with fields Thunderstore adds later (as it added
//! `installers`). Thunderstore ignores keys it doesn't know.
//!
//! ```json
//! {"name": "MyMod", "version_number": "1.0.0", "website_url": "", "description": "...",
//!  "dependencies": ["SomeTeam-AlchemistsLibrary-0.2.5"],
//!  "alchemists-tools": {"title": "My Mod", "description": "...", "author": "", "team": "SomeTeam",
//!     "dependencies": ["AlchemistsLibrary >= 0.2.5", "? Other"], "game-build": "25991404",
//!     "detours": []}}
//! ```
//!
//! Thunderstore's `dependencies` are the required mods only, each as `<team>-<name>-<version>`,
//! pinned to the version the mod was built against; each must be on Thunderstore before the zip is
//! uploaded. This tool's `dependencies` keep the full form: version ranges, and optional,
//! recommended and incompatible mods, which Thunderstore can't express.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde_json::json;

use crate::files::{hash, FileRecord, GameFiles};
use crate::game::display_path;
use crate::mods::{
    parse_version, read_root_manifest, requirement_text, version_text, Entry, Kind, Mod, Workspace,
    ENTRY_FILE,
};
use crate::report::{note, warning};

/// Thunderstore's upload limits.
const DESCRIPTION_LIMIT: usize = 250;
const MARKDOWN_LIMIT: usize = 100_000;
const ICON_SIZE: u32 = 256;
const PACKAGE_LIMIT: u64 = 500 * 1024 * 1024;

/// A Thunderstore team name: letters, digits and `_`, not starting or ending with `_`.
pub fn is_team(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && !text.starts_with('_')
        && !text.ends_with('_')
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// A description cut to Thunderstore's length, at a word.
fn short_description(text: &str) -> String {
    if text.chars().count() <= DESCRIPTION_LIMIT {
        return text.to_string();
    }
    let cut: String = text.chars().take(DESCRIPTION_LIMIT - 1).collect();
    let cut = cut
        .rsplit_once(' ')
        .map_or(cut.as_str(), |(start, _)| start);
    format!("{}…", cut.trim_end_matches([',', ';', ':', '.', ' ']))
}

/// Text Thunderstore accepts as markdown: UTF-8 without a byte order mark, and not too long.
fn markdown(text: &str, at: &str) -> Result<String> {
    let text = text.trim_start_matches('\u{feff}').to_string();
    if text.chars().count() > MARKDOWN_LIMIT {
        bail!("{at} is longer than Thunderstore's {MARKDOWN_LIMIT} characters");
    }
    Ok(text)
}

/// The mod's icon: its thumbnail cut to a 256x256 square, or else a plain one of a colour of its own.
fn icon(mod_: &Mod) -> Result<Vec<u8>> {
    let picture = match mod_.thumbnail() {
        Some(path) => image::open(&path)
            .with_context(|| format!("reading {}", display_path(&path)))?
            .resize_to_fill(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3),
        None => {
            warning!("no thumbnail.png in the mod folder: the zip's icon.png is a plain square");
            plain_icon(&mod_.name)
        }
    };
    let mut bytes = vec![];
    picture.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )?;
    Ok(bytes)
}

fn plain_icon(name: &str) -> image::DynamicImage {
    // the same colour for a name every time, as the GUI's own placeholder
    let hash = name.bytes().fold(2166136261u32, |hash, byte| {
        (hash ^ byte as u32).wrapping_mul(16777619)
    });
    let colour = |value: f32| -> [u8; 3] {
        let hue = (hash % 360) as f32 / 60.0;
        let saturation = 0.45;
        let chroma = value * saturation;
        let second = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
        let (red, green, blue) = match hue as u32 {
            0 => (chroma, second, 0.0),
            1 => (second, chroma, 0.0),
            2 => (0.0, chroma, second),
            3 => (0.0, second, chroma),
            4 => (second, 0.0, chroma),
            _ => (chroma, 0.0, second),
        };
        let base = value - chroma;
        [red, green, blue].map(|part| ((part + base) * 255.0) as u8)
    };
    let (outer, inner) = (colour(0.55), colour(0.75));
    let picture = image::RgbaImage::from_fn(ICON_SIZE, ICON_SIZE, |x, y| {
        let border = 40;
        let inside =
            (border..ICON_SIZE - border).contains(&x) && (border..ICON_SIZE - border).contains(&y);
        let [red, green, blue] = if inside { inner } else { outer };
        image::Rgba([red, green, blue, 255])
    });
    image::DynamicImage::ImageRgba8(picture)
}

/// The homepage, if it's a link Thunderstore accepts.
fn website(homepage: &str) -> String {
    let accepted = ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| homepage.starts_with(scheme));
    if homepage.is_empty() || accepted {
        homepage.to_string()
    } else {
        warning!(
            "homepage {homepage:?} isn't an http(s) or mailto link, so Thunderstore's website_url \
             is left empty"
        );
        String::new()
    }
}

impl Workspace {
    /// Each required mod as Thunderstore names it, `<team>-<name>-<version>`: at the version this
    /// mod builds against, with the team from that mod's mod.json (or its zip). A mod whose team
    /// isn't known is left out, with a warning.
    fn thunderstore_dependencies(&self, mod_: &Mod) -> Result<Vec<String>> {
        let mut listed = vec![];
        for dependency in mod_
            .requires
            .iter()
            .filter(|dependency| dependency.kind.is_required())
        {
            let found = self.find_dependency(dependency, &mod_.name)?;
            let team = match self.local_folder(&dependency.name) {
                Some(folder) => Mod::load(&folder)?.team,
                None => self
                    .archives()
                    .get(&dependency.name)
                    .and_then(|archive| read_root_manifest(&archive.path))
                    .and_then(|manifest| {
                        manifest["alchemists-tools"]["team"]
                            .as_str()
                            .map(str::to_string)
                    })
                    .unwrap_or_default(),
            };
            if team.is_empty() {
                warning!(
                    "{}'s Thunderstore team isn't known (no \"team\" in its mod.json), so it's left \
                     out of manifest.json's dependencies",
                    dependency.name
                );
                continue;
            }
            listed.push(format!("{team}-{}-{}", dependency.name, found.version));
        }
        Ok(listed)
    }

    /// Zip a built mod for sharing (see the module's comment): `<build>/<name>_<version>.zip`, and
    /// its README beside it, for a mod site's page.
    pub fn dist(&self, mod_: &Mod, build_folder: &Path, info: &Entry) -> Result<PathBuf> {
        let name = &mod_.name;
        if version_text(parse_version(&mod_.version, name)?) != mod_.version {
            bail!(
                "version {} must be written plainly for Thunderstore, as {}",
                mod_.version,
                version_text(parse_version(&mod_.version, name)?)
            );
        }
        if mod_.team.is_empty() {
            warning!(
                "no \"team\" in mod.json: give the Thunderstore team that publishes it, so mods \
                 that require it can name it"
            );
        }
        let description = short_description(&mod_.description);
        if description != mod_.description {
            warning!(
                "the description is longer than Thunderstore's {DESCRIPTION_LIMIT} characters: \
                 manifest.json has it cut short, and README.md in full"
            );
        }
        let manifest = json!({
            "name": name,
            "version_number": mod_.version,
            "website_url": website(&mod_.homepage),
            "description": description,
            "dependencies": self.thunderstore_dependencies(mod_)?,
            "alchemists-tools": {
                "title": mod_.title,
                "description": mod_.description,
                "author": mod_.author,
                "team": mod_.team,
                "contact": mod_.contact,
                "dependencies": mod_.dependencies,
                "game-build": mod_.game_build.clone().or(info.game_build.clone()),
                "detours": mod_.detours.clone().unwrap_or_default(),
            },
        });
        // the required mods whose table edits are in this mod's own data container too
        let mut with_tables = vec![];
        for dependency in mod_
            .requires
            .iter()
            .filter(|dependency| dependency.kind.is_required())
        {
            if !self.find_dependency(dependency, name)?.edits.is_empty() {
                with_tables.push(dependency.name.clone());
            }
        }
        let readme = markdown(&Self::readme_for_zip(mod_, info, &with_tables), "README.md")?;
        let changelog = match std::fs::read_to_string(mod_.folder.join("CHANGELOG.md")) {
            Ok(text) => Some(markdown(&text, "CHANGELOG.md")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("CHANGELOG.md must be UTF-8 text");
            }
        };

        let path = self.build.join(format!("{name}_{}.zip", mod_.version));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut add = |file: &str, contents: &[u8]| -> Result<()> {
            zip.start_file(file, options)?;
            zip.write_all(contents)?;
            Ok(())
        };
        add(
            "manifest.json",
            (serde_json::to_string_pretty(&manifest)? + "\n").as_bytes(),
        )?;
        add("icon.png", &icon(mod_)?)?;
        add("README.md", readme.as_bytes())?;
        if let Some(changelog) = &changelog {
            add("CHANGELOG.md", changelog.as_bytes())?;
        }
        add(
            ENTRY_FILE,
            (serde_json::to_string_pretty(info)? + "\n").as_bytes(),
        )?;
        let mut listed = vec![];
        let mut total = 0;
        for relative in info.data_files.iter().chain(&info.files) {
            add(relative, &std::fs::read(build_folder.join(relative))?)?;
            let (size, sha256) = hash(&build_folder.join(relative))?;
            total += size;
            listed.push(FileRecord {
                path: relative.clone(),
                size,
                sha256,
            });
        }
        // The mod names itself and its files, so a tool can tell it apart and remove it exactly
        let file_manifest = Self::manifest_for(info, "zip", listed);
        add(
            &GameFiles::manifest_path(name),
            (serde_json::to_string_pretty(&file_manifest)? + "\n").as_bytes(),
        )?;
        zip.finish()?;
        if total > PACKAGE_LIMIT {
            warning!("the zip is larger than Thunderstore's usual limit of 500 MB");
        }
        note!("zipped -> {}", display_path(&path));
        // and beside it, for a mod site's page
        let beside = self
            .build
            .join(format!("{name}_{}_README.md", mod_.version));
        std::fs::write(&beside, &readme)?;
        note!("README -> {}", display_path(&beside));
        Ok(path)
    }

    /// The README for a mod's zip, as markdown: what it is, what it needs, and how to install and
    /// uninstall it with this tool or by hand. `with_tables`: the mods it requires that edit
    /// tables, whose edits its own data container includes.
    pub fn readme_for_zip(mod_: &Mod, info: &Entry, with_tables: &[String]) -> String {
        let mut by_kind: IndexMap<&str, Vec<String>> = IndexMap::new();
        for dependency in &mod_.requires {
            let heading = match dependency.kind {
                Kind::Required | Kind::Unordered => "Requires",
                Kind::Recommended => "Recommended",
                Kind::Optional => "Works with",
                Kind::Incompatible => "Doesn't work with",
                Kind::HiddenOptional => continue,
            };
            by_kind
                .entry(heading)
                .or_default()
                .push(requirement_text(dependency));
        }
        let mut lines = vec![format!("# {} {}", mod_.title, mod_.version), String::new()];
        if !mod_.description.is_empty() {
            lines.extend([mod_.description.clone(), String::new()]);
        }
        if !mod_.author.is_empty() {
            lines.push(format!("- By {}", mod_.author));
        }
        if !mod_.homepage.is_empty() {
            lines.push(format!("- Website: {}", mod_.homepage));
        }
        if !mod_.contact.is_empty() {
            lines.push(format!("- Contact: {}", mod_.contact));
        }
        lines.push(format!(
            "- Made for Alchemy Factory build {} (Steam). After a game update it may need a new \
             version.",
            mod_.game_build
                .as_deref()
                .or(info.game_build.as_deref())
                .unwrap_or("(unknown)")
        ));
        for heading in ["Requires", "Recommended", "Works with", "Doesn't work with"] {
            if let Some(names) = by_kind.get(heading) {
                lines.push(format!("- {heading}: {}", names.join(", ")));
            }
        }
        lines.extend(["".into(), "## Install".into(), "".into()]);
        if info.needs_ue4ss {
            lines.extend([
                "It needs [UE4SS](https://github.com/UE4SS-RE/RE-UE4SS/releases).".to_string(),
                String::new(),
            ]);
        }
        lines.extend([
            "**With Alchemist's Tools:** put this zip in your mods folder, then tick the mod and \
             click Confirm (or run `alchemists-tools mod install "
                .to_string()
                + &mod_.name
                + "`). Its changes to the game's tables are merged with your other mods'.",
            String::new(),
            "**By hand:** copy the zip's `AlchemyFactory` folder into the game's folder (in Steam: \
             right-click Alchemy Factory > Manage > Browse local files), so that it merges with \
             the game's own `AlchemyFactory` folder. Then start the game."
                .to_string(),
            String::new(),
        ]);
        if !info.tables.is_empty() {
            lines.push(format!(
                "It replaces these game tables: {}. Installed by hand, another mod that replaces \
                 one of them can't be used at the same time; Alchemist's Tools merges them.",
                info.tables
                    .iter()
                    .map(|table| format!("`{table}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            lines.push(String::new());
        }
        if !info.save_edits.is_empty() {
            lines.extend([
                "It changes the save that new games start from, so it only affects games started \
                 with it installed."
                    .to_string(),
                String::new(),
            ]);
        }
        if !info.tables.is_empty() && !with_tables.is_empty() {
            lines.push(format!(
                "Its tables already include the table changes of {}. Installed by hand, delete \
                 their own table files after installing them, or they replace this mod's (keep \
                 their other files):",
                with_tables.join(", ")
            ));
            for name in with_tables {
                lines.push(format!(
                    "- `AlchemyFactory/Content/Paks/~mods/zzz_{name}_P.pak`, `.ucas` and `.utoc`"
                ));
            }
            lines.push(String::new());
        }
        lines.push("It adds these files:".to_string());
        lines.push(String::new());
        lines.extend(
            info.data_files
                .iter()
                .chain(&info.files)
                .map(|file| format!("- `{file}`")),
        );
        lines.extend(["".into(), "## Uninstall".into(), "".into()]);
        if !info.new_buildings.is_empty() {
            lines.extend([
                format!(
                    "First, in every save you want to keep, deconstruct every {}. A save with one \
                     placed crashes the game once the mod is removed.",
                    info.new_buildings.join(", ")
                ),
                String::new(),
            ]);
        }
        lines.extend([
            "**With Alchemist's Tools:** untick it and click Confirm.".to_string(),
            String::new(),
            format!(
                "**By hand:** delete the files listed above, and \
                 `AlchemyFactory/Content/Paks/~mods/manifests/{}.json`.",
                mod_.name
            ),
            String::new(),
        ]);
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thunderstore_rules() {
        assert!(super::is_team("Toby_Ticehurst") && super::is_team("A1"));
        for bad in ["", "_Toby", "Toby_", "Toby Ticehurst", "Toby-T"] {
            assert!(!super::is_team(bad), "{bad}");
        }
        let long = "word ".repeat(80);
        let short = super::short_description(&long);
        assert!(short.chars().count() <= super::DESCRIPTION_LIMIT && short.ends_with('…'));
        assert_eq!(super::short_description("Short."), "Short.");
    }
}
