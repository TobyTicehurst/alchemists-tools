//! Reading files out of the game's containers, in memory.
//!
//! Most assets are in IoStore containers (`.utoc`/`.ucas`) in the "zen" format; retoc converts
//! them to legacy `.uasset`/`.uexp` bytes, which is what the decoders read. Loose files such as
//! `Game.locres` are in the `.pak`, read with repak.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use retoc::asset_conversion::{build_legacy, FZenPackageContext};
use retoc::logging::{Log, LogBackend, LogLevel};
use retoc::zen::{EUnrealEngineObjectUE5Version, FPackageFileVersion};
use retoc::{EIoChunkType, FIoChunkId, FileWriterTrait, UEPath};

use crate::game::display_path;
use crate::reader::Package;

/// The game's package version (what retoc detects for its containers). Cooked packages don't
/// record it, so the same version is used to convert them and to parse the converted headers.
pub fn package_version() -> FPackageFileVersion {
    FPackageFileVersion::create_ue5(EUnrealEngineObjectUE5Version::OsSubObjectShadowSerialization)
}

/// Converted assets, each found by its path without extension
/// (e.g. `AlchemyFactory/Content/DataTables/DT_Enemies`).
#[derive(Default)]
pub struct Assets {
    /// File path (with extension) -> bytes.
    files: BTreeMap<String, Vec<u8>>,
    /// Asset path -> class of its main object, e.g. `DataTable`.
    classes: BTreeMap<String, String>,
}

impl Assets {
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    pub fn get(&self, path: &str) -> Result<&[u8]> {
        self.files
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("{path} not found in the game files"))
    }

    /// The `.uasset` and `.uexp` of the asset at `path`.
    pub fn asset(&self, path: &str) -> Result<(&[u8], &[u8])> {
        Ok((
            self.get(&format!("{path}.uasset"))?,
            self.get(&format!("{path}.uexp"))?,
        ))
    }

    /// Paths of the assets whose name is `name`.
    pub fn find(&self, name: &str) -> Vec<&str> {
        let suffix = format!("/{name}");
        self.classes
            .keys()
            .filter(|path| path.ends_with(&suffix))
            .map(String::as_str)
            .collect()
    }

    /// The class of the main object of the asset at `path`.
    pub fn class(&self, path: &str) -> Option<&str> {
        self.classes.get(path).map(String::as_str)
    }

    /// Paths of the assets whose main object has one of `classes`.
    pub fn of_class(&self, classes: &[&str]) -> Vec<&str> {
        let wanted = |class: &String| classes.contains(&class.as_str());
        self.classes
            .iter()
            .filter(|(_, class)| wanted(class))
            .map(|(path, _)| path.as_str())
            .collect()
    }
}

/// retoc's messages, kept off stdout (which may be JSON output).
struct Stderr;

impl LogBackend for Stderr {
    fn write_message(&self, level: LogLevel, message: &str) {
        eprintln!("{level}{message}");
    }
}

/// Collects the files retoc writes for one package.
#[derive(Default)]
struct PackageWriter(Mutex<Vec<(String, Vec<u8>)>>);

impl FileWriterTrait for PackageWriter {
    fn write_file(
        &self,
        path: String,
        _allow_compression: bool,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((path, data));
        Ok(())
    }
}

/// Convert to legacy format every package whose path (e.g.
/// `AlchemyFactory/Content/DataTables/DT_Enemies.uasset`) passes `include_path` and whose main
/// object's class passes `include_class`. Only those are kept in memory.
pub fn legacy_assets(
    paks: &Path,
    include_path: impl Fn(&str) -> bool,
    include_class: impl Fn(&str) -> bool,
) -> Result<Assets> {
    let store = retoc::iostore::open(paks, Arc::new(retoc::Config::default()))
        .with_context(|| format!("opening the game containers in {}", display_path(paks)))?;
    let log = Log::new(LogLevel::Warning, Arc::new(Stderr));
    let verse_cells = Some(retoc::build_verse_cell_store(&vec![]));
    let context = FZenPackageContext::create(&*store, Some(package_version()), &log, verse_cells);
    let mut assets = Assets::default();
    for package in store.packages() {
        let chunk = FIoChunkId::from_package_id(package.id(), 0, EIoChunkType::ExportBundleData);
        let Some(path) = store.chunk_path(chunk) else {
            continue;
        };
        let path = path.strip_prefix("../../../").unwrap_or(&path); // container mount point
        if !include_path(path) {
            continue;
        }
        let writer = PackageWriter::default();
        // One bad package (e.g. after a game update) shouldn't stop the rest.
        if let Err(error) = build_legacy(&context, package.id(), UEPath::new(path), &writer) {
            eprintln!("warning: skipped {path}: couldn't convert it: {error:#}");
            continue;
        }
        let files = writer.0.into_inner().unwrap();
        let Some((_, uasset)) = files.iter().find(|(file, _)| file.ends_with(".uasset")) else {
            continue;
        };
        let class = match Package::read(uasset) {
            Ok(package) => package.export_classes.first().cloned().unwrap_or_default(),
            Err(error) => {
                eprintln!("warning: skipped {path}: couldn't read its header: {error:#}");
                continue;
            }
        };
        if include_class(&class) {
            let asset_path = path.strip_suffix(".uasset").unwrap_or(path).to_string();
            assets.classes.insert(asset_path, class);
            assets.files.extend(files);
        }
    }
    Ok(assets)
}

/// One file from a `.pak`, by its path below the mount point.
pub fn pak_file(pak: &Path, path: &str) -> Result<Vec<u8>> {
    let file =
        std::fs::File::open(pak).with_context(|| format!("opening {}", display_path(pak)))?;
    let mut file = std::io::BufReader::new(file);
    let reader = repak::PakBuilder::new().reader(&mut file)?;
    reader
        .get(path, &mut file)
        .with_context(|| format!("reading {path} from {}", display_path(pak)))
}
