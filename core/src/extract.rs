//! Game files -> `data/`, all in memory:
//!
//! 1. unpack:  every DataTable, curve and string table under `AlchemyFactory/Content/`, and the
//!    English `Game.locres` from the .pak
//! 2. decode:  each DataTable and curve, using the layouts in the .usmap ->
//!    `<its path in the game>.json`, texts in English
//! 3. schemas: the layouts that were used -> schemas.json

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::datatable::{read_datatable, read_object};
use crate::game::{display_path, join, Game};
use crate::text::Localizer;
use crate::unpack::{legacy_assets, pak_file, Assets};
use crate::usmap::read_usmap;

pub const TABLE_CLASSES: &[&str] = &["DataTable", "CompositeDataTable"];
pub const CURVE_CLASSES: &[&str] = &["CurveFloat", "CurveLinearColor"];
pub const STRING_TABLE_CLASSES: &[&str] = &["StringTable"];

/// The game's own content (engine and plugin assets are outside it).
const CONTENT: &str = "AlchemyFactory/Content/";
const LOCRES: &str = "AlchemyFactory/Content/Localization/Game/en/Game.locres";

/// Every DataTable, curve and string table in the game's content, converted to legacy format.
pub fn content_assets(game: &Game) -> Result<Assets> {
    let wanted = [TABLE_CLASSES, CURVE_CLASSES, STRING_TABLE_CLASSES].concat();
    let assets = legacy_assets(
        &game.paks(),
        |path| path.starts_with(CONTENT),
        |class| wanted.contains(&class),
    )?;
    if assets.is_empty() {
        bail!("no DataTables found in {}", display_path(&game.paks()));
    }
    Ok(assets)
}

/// The DataTables anywhere in the game named `name` (e.g. `DT_Enemies`), converted to legacy
/// format.
pub fn named_tables(game: &Game, name: &str) -> Result<Assets> {
    let file = format!("/{name}.uasset");
    legacy_assets(
        &game.paks(),
        |path| path.ends_with(&file),
        |class| TABLE_CLASSES.contains(&class),
    )
}

/// Translations from the string tables and the English `Game.locres`.
pub fn english(game: &Game, assets: &Assets) -> Result<Localizer> {
    let mut tables = vec![];
    for path in assets.of_class(STRING_TABLE_CLASSES) {
        let (uasset, uexp) = assets.asset(path)?;
        tables.push((path, uasset, uexp));
    }
    Localizer::new(tables, &pak_file(&game.pak(), LOCRES)?)
}

/// `AlchemyFactory/Content/DataTables/DT_Enemies` -> `<out>/DataTables/DT_Enemies.json`
fn output_path(out: &Path, asset_path: &str) -> PathBuf {
    let relative = asset_path.strip_prefix(CONTENT).unwrap_or(asset_path);
    let folders: Vec<&str> = relative.split('/').collect();
    join(out, &folders).with_extension("json")
}

fn write_json(value: &serde_json::Value, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", display_path(parent)))?;
    }
    crate::json::write(value, path)
}

pub fn extract(game: &Game, usmap: &Path, out: &Path) -> Result<()> {
    let assets = content_assets(game)?;
    println!("unpacked {} files", assets.len());
    let localizer = english(game, &assets)?;
    let mappings = read_usmap(usmap)?;
    std::fs::create_dir_all(out).with_context(|| format!("creating {}", display_path(out)))?;

    let (mut written, mut skipped) = (0, 0);
    let mut used_structs: Vec<String> = [TABLE_CLASSES, CURVE_CLASSES]
        .concat()
        .iter()
        .map(|class| class.to_string())
        .collect();
    let objects = [
        assets.of_class(TABLE_CLASSES),
        assets.of_class(CURVE_CLASSES),
    ]
    .concat();
    for path in objects {
        let relative = path.strip_prefix(CONTENT).unwrap_or(path);
        let decoded = assets.asset(path).and_then(|asset| {
            if assets
                .class(path)
                .is_some_and(|class| TABLE_CLASSES.contains(&class))
            {
                read_datatable(asset, &mappings, Some(&localizer))
            } else {
                read_object(asset, &mappings, Some(&localizer))
            }
        });
        match decoded {
            Ok(value) => {
                write_json(&value, &output_path(out, path))?;
                match value["rows"].as_object() {
                    Some(rows) => println!("{relative}: {} rows", rows.len()),
                    None => println!("{relative}"),
                }
                if let Some(row_struct) = value["row_struct"].as_str() {
                    if !used_structs.iter().any(|used| used == row_struct) {
                        used_structs.push(row_struct.to_string());
                    }
                }
                written += 1;
            }
            Err(error) => {
                eprintln!("warning: skipped {relative}: {error:#}");
                skipped += 1;
            }
        }
    }

    let used_structs: Vec<&str> = used_structs.iter().map(String::as_str).collect();
    let schemas = mappings.subset(&used_structs);
    crate::json::write(&schemas, &out.join("schemas.json"))?;
    written += 1;
    println!(
        "schemas: {} structs, {} enums",
        schemas.structs.len(),
        schemas.enums.len()
    );

    println!("wrote {written} files, skipped {skipped}");
    Ok(())
}
