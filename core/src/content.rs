//! Packaging content cooked in the stub Unreal project (Blueprints, meshes, textures) as a mod
//! container, and checking it.
//!
//! The editor cooks to `<project>/Saved/Cooked/Windows/AlchemyFactory/Content/`. Only the given
//! `/Game/` folders or assets are packed, never the stand-ins under `/Game/Arts` (placeholders at
//! the game's own paths, which the game's real assets replace). Editor cooks are real UE 5.7
//! packages, so they're packed as 5.7.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde_json::Value;

use crate::blueprint::{self, Component};
use crate::game::{display_path, join, Game};
use crate::package::{convert_back, script_objects, write_container, LegacyAsset};
use crate::schema::Schemas;

const COOKED_EXTENSIONS: &[&str] = &["uasset", "uexp", "ubulk"];

/// The stub project's cooked content folder.
pub fn cooked_content(project: &Path) -> PathBuf {
    let folder = project.parent().unwrap_or(Path::new("."));
    join(
        folder,
        &["Saved", "Cooked", "Windows", "AlchemyFactory", "Content"],
    )
}

/// Stop on a path that would pack a stand-in or replace a game asset by accident.
pub fn check_paths(paths: &[String], allow_game_paths: bool) -> Result<()> {
    for path in paths {
        if !path.starts_with("/Game/") {
            bail!("{path}: give a /Game/ path, e.g. /Game/Mods/MyMachine");
        }
        if path.starts_with("/Game/Arts") {
            bail!(
                "{path}: /Game/Arts holds the stand-ins for the game's own assets; they must \
                never be packaged"
            );
        }
        if !path.starts_with("/Game/Mods/") && !allow_game_paths {
            bail!(
                "{path}: outside /Game/Mods it would replace a game asset; pass \
                --allow-game-paths to mean it"
            );
        }
    }
    Ok(())
}

/// `/Game/Mods/X` -> `<root>/Mods/X`.
fn below(root: &Path, game_path: &str) -> PathBuf {
    let relative: Vec<&str> = game_path["/Game/".len()..]
        .trim_matches('/')
        .split('/')
        .collect();
    join(root, &relative)
}

fn files_with(folder_or_asset: &Path, extension: &str) -> Vec<PathBuf> {
    if folder_or_asset.is_dir() {
        let mut out = vec![];
        let mut folders = vec![folder_or_asset.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|found| found == extension) {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    } else {
        let file = folder_or_asset.with_extension(extension);
        if file.exists() {
            vec![file]
        } else {
            vec![]
        }
    }
}

/// Assets saved in the editor after their last cook, or never cooked.
pub fn stale_cooks(project: &Path, game_path: &str) -> Vec<String> {
    let content = join(project.parent().unwrap_or(Path::new(".")), &["Content"]);
    let sources = below(&content, game_path);
    let modified = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    };
    files_with(&sources, "uasset")
        .into_iter()
        .filter_map(|source| {
            let relative = source.strip_prefix(&content).ok()?.to_path_buf();
            let shown = relative.to_string_lossy().replace('\\', "/");
            let cooked_file = cooked_content(project).join(&relative);
            match (modified(&source), modified(&cooked_file)) {
                (_, None) => Some(format!("{shown} (never cooked)")),
                (Some(saved), Some(cooked_at)) if cooked_at < saved => {
                    Some(format!("{shown} (saved after the last cook)"))
                }
                _ => None,
            }
        })
        .collect()
}

/// The cooked assets of the given `/Game/` folders or assets.
pub fn stage(project: &Path, paths: &[String]) -> Result<Vec<LegacyAsset>> {
    let content = cooked_content(project);
    let mut out = vec![];
    for game_path in paths {
        let found = files_with(&below(&content, game_path), "uasset");
        if found.is_empty() {
            bail!(
                "nothing cooked for {game_path} in {} (cook first)",
                display_path(&content)
            );
        }
        for uasset in found {
            if uasset.with_extension("uptnl").exists() {
                bail!(
                    "{}: optional bulk data (.uptnl) isn't supported",
                    display_path(&uasset)
                );
            }
            let read = |extension: &str| std::fs::read(uasset.with_extension(extension));
            let relative = uasset.strip_prefix(&content).unwrap().with_extension("");
            let path = format!(
                "AlchemyFactory/Content/{}",
                relative.to_string_lossy().replace('\\', "/")
            );
            out.push(LegacyAsset {
                path,
                uasset: read(COOKED_EXTENSIONS[0])
                    .with_context(|| format!("reading {}", display_path(&uasset)))?,
                uexp: read(COOKED_EXTENSIONS[1])
                    .with_context(|| format!("reading the .uexp of {}", display_path(&uasset)))?,
                ubulk: read(COOKED_EXTENSIONS[2]).ok(),
            });
        }
    }
    Ok(out)
}

/// A craft type by name (`Max`) or number (`26`) -> its number.
pub fn craft_type_number(value: &str, schemas: &Schemas) -> Result<u8> {
    let names = schemas
        .enums
        .get("EBeltTDCraftType")
        .context("no EBeltTDCraftType in the .usmap")?;
    if let Some((number, _)) = names.iter().find(|(_, name)| name.as_str() == value) {
        return Ok(number.parse()?);
    }
    match value.parse::<u8>() {
        Ok(number) => Ok(number),
        Err(_) => bail!(
            "craft type {value:?} is not one of {} or a number 0-255",
            names.values().cloned().collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Set a cooked Blueprint's FactoryCraftType. Its CraftFacility template stores only that property:
/// a header for property 1 alone (`01 03`), the byte, then 8 zero bytes. Returns the old value.
pub fn patch_craft_type(asset: &mut LegacyAsset, value: u8) -> Result<u8> {
    let data = &mut asset.uexp;
    let hits: Vec<usize> = (0..data.len().saturating_sub(11))
        .filter(|&i| {
            data[i] == 1 && data[i + 1] == 3 && data[i + 3..i + 11].iter().all(|&b| b == 0)
        })
        .collect();
    if hits.len() != 1 {
        bail!(
            "{}: expected one CraftFacility template with a craft type set, found {} (set any \
            craft \
             type other than TableSaw in the editor)",
            asset.path,
            hits.len()
        );
    }
    let old = data[hits[0] + 2];
    data[hits[0] + 2] = value;
    Ok(old)
}

/// What a content container holds, read back from it.
pub struct Packaged {
    /// Each package's top-level objects: object path -> class path.
    pub assets: IndexMap<String, String>,
    /// Each Blueprint's components, by package (`/Game/...`).
    pub components: IndexMap<String, Vec<Component>>,
}

/// Pack staged content into `utoc` and check it: it must convert back, every reference must resolve
/// to a game asset or one in the container, and each Blueprint given a craft type must have it.
pub fn package(
    utoc: &Path,
    staged: &[LegacyAsset],
    game: &Game,
    schemas: &Schemas,
    craft_types: &IndexMap<String, u8>,
) -> Result<Packaged> {
    write_container(
        utoc,
        staged,
        retoc::version::EngineVersion::UE5_7,
        script_objects(game)?,
    )?;
    let paths: Vec<String> = staged.iter().map(|asset| asset.path.clone()).collect();
    let (back, linked) = convert_back(utoc, &paths, game)?;
    let mut problems = vec![];
    let mut packaged = Packaged {
        assets: IndexMap::new(),
        components: IndexMap::new(),
    };
    for path in &paths {
        let game_path = format!("/Game/{}", &path["AlchemyFactory/Content/".len()..]);
        let Ok((uasset, uexp)) = back.asset(path) else {
            problems.push(format!("{game_path}: not in the container"));
            continue;
        };
        let package = crate::reader::Package::read(uasset)?;
        let unresolved = package
            .import_paths
            .iter()
            .filter(|path| path.contains("UnknownPackage") || path.contains("UnknownExport"))
            .count();
        if unresolved > 0 && linked {
            problems.push(format!(
                "{game_path}: {unresolved} reference(s) to assets that exist neither in the game \
                nor \
                 in this mod (a stand-in at the wrong path?)"
            ));
        }
        packaged.assets.extend(blueprint::assets(uasset)?);
        let components = blueprint::components(uasset, uexp, schemas)?;
        let asset_name = game_path.rsplit('/').next().unwrap_or_default();
        if let Some(&wanted) = craft_types.get(asset_name) {
            let found: Vec<&Value> = components
                .iter()
                .filter(|component| component.class == "CraftFacilityComponent")
                .map(|component| &component.values["FactoryCraftType"])
                .collect();
            let expected = crate::datatable::enum_name(
                schemas,
                &crate::schema::Property {
                    enum_name: Some("EBeltTDCraftType".into()),
                    ..Default::default()
                },
                wanted as i64,
            );
            if found != [&expected] {
                problems.push(format!(
                    "{game_path}: craft type is {found:?}, expected {expected}"
                ));
            }
        }
        packaged.components.insert(game_path, components);
    }
    if !problems.is_empty() {
        bail!("check failed:\n   {}", problems.join("\n   "));
    }
    Ok(packaged)
}

/// One line per component: its mesh, materials, craft type, collision, transform and tags.
pub fn describe(component: &Component) -> String {
    let values = &component.values;
    let last = |value: &Value| {
        value
            .as_str()
            .map(|path| path.rsplit('.').next().unwrap_or(path).to_string())
    };
    let mut details: Vec<String> = vec![];
    if let Some(mesh) = values.get("StaticMesh").and_then(last) {
        details.push(mesh);
    }
    for material in values
        .get("OverrideMaterials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(material) = last(material) {
            details.push(format!("material {material}"));
        }
    }
    if let Some(craft_type) = values
        .get("FactoryCraftType")
        .filter(|value| !value.is_null())
    {
        details.push(format!(
            "craft type {}",
            craft_type
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| craft_type.to_string())
        ));
    }
    if let Some(profile) = values
        .get("BodyInstance")
        .and_then(|body| body.get("CollisionProfileName"))
        .and_then(Value::as_str)
    {
        details.push(format!("collision {profile}"));
    }
    for field in ["RelativeLocation", "RelativeRotation", "RelativeScale3D"] {
        if let Some(object) = values.get(field).and_then(Value::as_object) {
            let numbers: Vec<String> = object
                .values()
                .map(|v| format!("{}", (v.as_f64().unwrap_or(0.0) * 1000.0).round() / 1000.0))
                .collect();
            details.push(format!("{field} [{}]", numbers.join(", ")));
        }
    }
    if let Some(tags) = values
        .get("ComponentTags")
        .and_then(Value::as_array)
        .filter(|tags| !tags.is_empty())
    {
        let tags: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
        details.push(format!("tags {tags:?}"));
    }
    let parent = component
        .parent
        .as_ref()
        .map(|parent| format!(" (in {parent})"))
        .unwrap_or_default();
    let shown = if details.is_empty() {
        component.class.clone()
    } else {
        details.join(", ")
    };
    format!("{}{parent}: {shown}", component.name)
}
