//! Writing mod containers: legacy `.uasset`/`.uexp` assets -> an IoStore container
//! (`.utoc`/`.ucas`) and the empty `.pak` the game needs beside it to load it, all from memory.
//!
//! This is retoc's `to-zen` command, for assets that are already in memory and have no shaders.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use retoc::iostore_writer::IoStoreWriter;
use retoc::legacy_asset::FSerializedAssetBundle;
use retoc::logging::{Log, LogLevel};
use retoc::script_objects::ZenScriptObjects;
use retoc::version::EngineVersion;
use retoc::zen_asset_conversion::build_zen_asset;
use retoc::UEPath;

use crate::game::{display_path, Game};
use crate::unpack::Assets;

/// The containers' mount point: package paths start below it (`AlchemyFactory/Content/...`).
const MOUNT_POINT: &str = "../../../";

/// One asset to pack.
pub struct LegacyAsset {
    /// Its path without extension, e.g. `AlchemyFactory/Content/DataTables/DT_Buildings`.
    pub path: String,
    pub uasset: Vec<u8>,
    pub uexp: Vec<u8>,
    pub ubulk: Option<Vec<u8>>,
}

/// The game's script objects (the native classes and their defaults), which converting a package
/// to zen resolves its `/Script/` imports against.
pub fn script_objects(game: &Game) -> Result<Arc<ZenScriptObjects>> {
    let store = retoc::iostore::open(game.paks(), Arc::new(retoc::Config::default()))
        .with_context(|| {
            format!(
                "opening the game containers in {}",
                display_path(&game.paks())
            )
        })?;
    Ok(Arc::new(store.load_script_objects()?))
}

/// Write `assets` as the container `<utoc>` (with its `.ucas`, and an empty `.pak` of the same
/// name). `version` is the engine version whose layout the assets' headers have: `UE5_6` for
/// tables written from retoc's conversion, `UE5_7` for the editor's cooks.
pub fn write_container(
    utoc: &Path,
    assets: &[LegacyAsset],
    version: EngineVersion,
    script_objects: Arc<ZenScriptObjects>,
) -> Result<()> {
    if let Some(parent) = utoc.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", display_path(parent)))?;
    }
    let mount_point = UEPath::new(MOUNT_POINT);
    let mut writer = IoStoreWriter::new(
        utoc,
        version.toc_version(),
        Some(version.container_header_version()),
        mount_point.into(),
    )?;
    let container_header_version = writer.container_header_version();
    let log = Log::new(LogLevel::Warning, Arc::new(crate::unpack::Stderr));
    let verse_cells = Some(retoc::build_verse_cell_store(&vec![]));
    for asset in assets {
        let bundle = FSerializedAssetBundle {
            asset_file_buffer: asset.uasset.clone(),
            exports_file_buffer: asset.uexp.clone(),
            bulk_data_buffer: asset.ubulk.clone(),
            optional_bulk_data_buffer: None,
            memory_mapped_bulk_data_buffer: None,
        };
        let path = mount_point.join(format!("{}.uasset", asset.path));
        let converted = build_zen_asset(
            bundle,
            &HashMap::new(),
            &path,
            Some(version.package_file_version()),
            container_header_version,
            false,
            Some(script_objects.clone()),
            verse_cells.clone(),
            &log,
        )
        .with_context(|| format!("converting {}", asset.path))?;
        let mut converted = converted;
        converted.write(&mut writer)?;
    }
    writer.finalize()?;

    // The game only loads a container that has a .pak beside it (an empty one will do)
    let pak = utoc.with_extension("pak");
    let file =
        std::fs::File::create(&pak).with_context(|| format!("creating {}", display_path(&pak)))?;
    repak::PakBuilder::new()
        .writer(
            &mut BufWriter::new(file),
            repak::Version::V11,
            MOUNT_POINT.to_string(),
            None,
        )
        .write_index()?;
    Ok(())
}

/// A written container converted back to legacy format, next to the game's own containers (hard
/// links, so every reference resolves); only `paths` (without extension) are converted. The flag
/// says whether the game's containers were there (on another drive only `global` is copied).
pub fn convert_back(utoc: &Path, paths: &[String], game: &Game) -> Result<(Assets, bool)> {
    let folder = utoc.with_extension("check");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder)
        .with_context(|| format!("creating {}", display_path(&folder)))?;
    let mut linked = true;
    for entry in std::fs::read_dir(game.paks())? {
        let file = entry?.path();
        if !file.is_file() {
            continue;
        }
        let target = folder.join(file.file_name().unwrap());
        if std::fs::hard_link(&file, &target).is_err() {
            // another drive: copy only the small global container, which every check needs
            if file.file_stem().is_some_and(|stem| stem == "global") {
                std::fs::copy(&file, &target)?;
            } else {
                linked = false;
            }
        }
    }
    for extension in ["utoc", "ucas", "pak"] {
        std::fs::copy(
            utoc.with_extension(extension),
            folder.join(utoc.with_extension(extension).file_name().unwrap()),
        )?;
    }
    let wanted: Vec<String> = paths.iter().map(|path| format!("{path}.uasset")).collect();
    let back =
        crate::unpack::legacy_assets(&folder, |path| wanted.contains(&path.to_string()), |_| true);
    let _ = std::fs::remove_dir_all(&folder);
    Ok((
        back.with_context(|| format!("converting {} back", display_path(utoc)))?,
        linked,
    ))
}

/// Check a written table container: each table must come back exactly as written.
pub fn check_container(utoc: &Path, assets: &[LegacyAsset], game: &Game) -> Result<()> {
    let paths: Vec<String> = assets.iter().map(|asset| asset.path.clone()).collect();
    let (back, linked) = convert_back(utoc, &paths, game)?;
    let result = (|| -> Result<()> {
        for asset in assets {
            let (uasset, uexp) = back.asset(&asset.path)?;
            if uexp != asset.uexp.as_slice() {
                anyhow::bail!("{}: its data differs after packing", asset.path);
            }
            if !linked || uasset == asset.uasset.as_slice() {
                continue;
            }
            // The header can only differ in imports the round trip can't name: assets of the mod
            // that aren't packed yet (e.g. its Blueprint) come back as /Engine/UnknownPackage
            let (written, read) = (
                crate::reader::Package::read(&asset.uasset)?,
                crate::reader::Package::read(uasset)?,
            );
            if written.import_paths.len() != read.import_paths.len() {
                anyhow::bail!("{}: its imports differ after packing", asset.path);
            }
            for (wrote, came_back) in written.import_paths.iter().zip(&read.import_paths) {
                if wrote != came_back
                    && !(wrote.starts_with("/Game/Mods/") && came_back.contains("UnknownPackage"))
                {
                    anyhow::bail!(
                        "{}: the import {wrote} came back as {came_back}",
                        asset.path
                    );
                }
            }
        }
        Ok(())
    })();
    result.with_context(|| format!("checking {}", display_path(utoc)))
}
