//! Write DataTables back to legacy `.uasset`/`.uexp` pairs, for mods.
//!
//! A table is decoded with the span of every value recorded ([`Spans`]). Writing re-encodes only
//! the rows that changed, and inside them copies every value that is still equal to the original,
//! byte for byte. So everything a mod doesn't touch keeps the cooker's exact bytes, including which
//! properties the unversioned header lists and which it marks as zero.
//!
//! Names and imports the changed rows need are added to the header, which is patched in place (its
//! layout is the one retoc's conversion to legacy writes). A new reference takes its class from the
//! field it's in: every object field of the game's tables holds one class (`DisplayIcon` a
//! `Texture2D`, `ScriptClass` a Blueprint class). A reference can also give its class the way the
//! editor's *Copy Reference* does: `/Script/Engine.Texture2D'/Game/Mods/X/T_Icon.T_Icon'`.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde_json::Value;

use crate::datatable::NATIVE_STRUCTS;
use crate::datatable::{enum_name, main_class, property_key, zero_value, Decoder, Spans};
use crate::reader::Package;
use crate::schema::{Property, Schemas};

const IMPORT_SIZE: usize = 32;
const EXPORT_SIZE: usize = 96;
const PACKAGE_TAG: u32 = 0x9E2A83C1;

// --- the package summary -------------------------------------------------------------------------

/// Summary fields that are 32-bit, in file order, after `PackageFlags`.
const INT_FIELDS: &[&str] = &[
    "NameCount",
    "NameOffset",
    "SoftObjectPathsCount",
    "SoftObjectPathsOffset",
    "GatherableTextDataCount",
    "GatherableTextDataOffset",
    "ExportCount",
    "ExportOffset",
    "ImportCount",
    "ImportOffset",
    "CellExportCount",
    "CellExportOffset",
    "CellImportCount",
    "CellImportOffset",
    "MetaDataOffset",
    "DependsOffset",
    "SoftPackageReferencesCount",
    "SoftPackageReferencesOffset",
    "SearchableNamesOffset",
    "ThumbnailTableOffset",
];

/// Offsets into the file, which move when bytes are inserted before them.
const OFFSETS: &[&str] = &[
    "TotalHeaderSize",
    "SoftObjectPathsOffset",
    "GatherableTextDataOffset",
    "ExportOffset",
    "ImportOffset",
    "CellExportOffset",
    "CellImportOffset",
    "MetaDataOffset",
    "DependsOffset",
    "SoftPackageReferencesOffset",
    "SearchableNamesOffset",
    "ThumbnailTableOffset",
    "AssetRegistryDataOffset",
    "BulkDataStartOffset",
    "WorldTileInfoDataOffset",
    "PreloadDependencyOffset",
    "PayloadTocOffset",
    "DataResourceOffset",
];

fn read_i32(data: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn read_i64(data: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

fn write_i32(data: &mut [u8], at: usize, value: i32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i64(data: &mut [u8], at: usize, value: i64) {
    data[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn change_i32(data: &mut [u8], at: usize, change: impl FnOnce(i32) -> i32) {
    let value = read_i32(data, at);
    write_i32(data, at, change(value));
}

fn change_i64(data: &mut [u8], at: usize, change: impl FnOnce(i64) -> i64) {
    let value = read_i64(data, at);
    write_i64(data, at, change(value));
}

/// A legacy `.uasset` header, with the positions of the summary fields it needs to patch.
struct Header {
    data: Vec<u8>,
    /// Field -> (position, whether it's 64-bit).
    fields: HashMap<&'static str, (usize, bool)>,
    /// Position of each generation's (ExportCount, NameCount) pair.
    generations: Vec<usize>,
}

/// One entry of the import table, with its names as strings.
#[derive(Clone)]
struct Import {
    class_package: String,
    class_name: String,
    outer: i32,
    name: String,
    flags: [u8; 4],
}

impl Header {
    fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 60 || read_i32(&data, 0) as u32 != PACKAGE_TAG || read_i32(&data, 4) != -9 {
            bail!("unexpected package summary");
        }
        let mut fields = HashMap::new();
        fields.insert("TotalHeaderSize", (44, false));
        if read_i32(&data, 48) != 0 {
            bail!("custom versions in the package summary aren't supported");
        }
        let mut position = 52 + 4 + read_i32(&data, 52) as usize + 4; // PackageName, PackageFlags
        let mut field = |name: &'static str, position: &mut usize, wide: bool| {
            fields.insert(name, (*position, wide));
            *position += if wide { 8 } else { 4 };
        };
        for name in INT_FIELDS {
            field(name, &mut position, false);
        }
        let generation_count = read_i32(&data, position) as usize;
        let generations = (0..generation_count)
            .map(|i| position + 4 + 8 * i)
            .collect();
        position += 4 + 8 * generation_count;
        for _ in 0..2 {
            // SavedByEngineVersion, CompatibleWithEngineVersion: 10 bytes, then the branch name
            position += 10;
            position += 4 + read_i32(&data, position) as usize;
        }
        // CompressionFlags, CompressedChunks, PackageSource, AdditionalPackagesToCook
        position += 4 * 4;
        field("AssetRegistryDataOffset", &mut position, false);
        field("BulkDataStartOffset", &mut position, true);
        field("WorldTileInfoDataOffset", &mut position, false);
        position += 4 + 4 * read_i32(&data, position) as usize; // ChunkIDs
        for name in [
            "PreloadDependencyCount",
            "PreloadDependencyOffset",
            "NamesReferencedFromExportDataCount",
        ] {
            field(name, &mut position, false);
        }
        field("PayloadTocOffset", &mut position, true);
        field("DataResourceOffset", &mut position, false);
        let header = Header {
            data,
            fields,
            generations,
        };
        if position as i64 != header.get("NameOffset")
            || header.get("TotalHeaderSize") as usize != header.data.len()
        {
            bail!("package summary layout not recognised");
        }
        Ok(header)
    }

    fn get(&self, name: &str) -> i64 {
        let (at, wide) = self.fields[name];
        if wide {
            read_i64(&self.data, at)
        } else {
            read_i32(&self.data, at) as i64
        }
    }

    fn set(data: &mut [u8], fields: &HashMap<&'static str, (usize, bool)>, name: &str, value: i64) {
        let (at, wide) = fields[name];
        if wide {
            write_i64(data, at, value);
        } else {
            write_i32(data, at, value as i32);
        }
    }

    fn reparse(self) -> Result<Self> {
        Header::parse(self.data)
    }

    /// Position of name map entry `index`.
    fn name_position(&self, index: usize) -> usize {
        let mut at = self.get("NameOffset") as usize;
        for _ in 0..index {
            at += 4 + read_i32(&self.data, at) as usize + 4;
        }
        at
    }

    fn export_offset(&self, i: usize) -> usize {
        self.get("ExportOffset") as usize + i * EXPORT_SIZE
    }

    /// Insert bytes at `at`, moving every offset at or after it and each export's data.
    fn insert_bytes(mut self, at: usize, bytes: &[u8]) -> Result<Self> {
        let length = bytes.len() as i64;
        let mut data = self.data.clone();
        for name in OFFSETS {
            let value = self.get(name);
            if value >= at as i64 {
                Header::set(&mut data, &self.fields, name, value + length);
            }
        }
        for i in 0..self.get("ExportCount") as usize {
            let serial_offset = self.export_offset(i) + 36;
            change_i64(&mut data, serial_offset, |offset| offset + length);
        }
        data.splice(at..at, bytes.iter().copied());
        self.data = data;
        self.reparse()
    }

    /// Insert names after those export data uses; only the later, header-only names move.
    fn insert_names(mut self, names: &[String]) -> Result<Self> {
        let count = self.get("NameCount");
        let at = self.get("NamesReferencedFromExportDataCount");
        let added = names.len() as i64;
        let mut blob = vec![];
        for name in names {
            if !name.is_ascii() {
                bail!("can't add the name {name:?} (only ASCII names are supported)");
            }
            blob.extend((name.len() as i32 + 1).to_le_bytes());
            blob.extend(name.as_bytes());
            blob.extend([0u8; 5]); // NUL, then the name's hashes (zero in these packages)
        }
        let position = self.name_position(at as usize);
        let mut data = self.data.clone();
        let remap = |index: i32| {
            if index as i64 >= at {
                index + added as i32
            } else {
                index
            }
        };
        for i in 0..self.get("ImportCount") as usize {
            let entry = self.get("ImportOffset") as usize + i * IMPORT_SIZE;
            for field in [0, 8, 20] {
                // ClassPackage, ClassName, ObjectName
                change_i32(&mut data, entry + field, remap);
            }
        }
        for i in 0..self.get("ExportCount") as usize {
            let entry = self.export_offset(i);
            change_i32(&mut data, entry + 16, remap); // ObjectName
            change_i64(&mut data, entry + 36, |offset| offset + blob.len() as i64);
            // SerialOffset
        }
        for name in OFFSETS {
            let value = self.get(name);
            if value >= position as i64 {
                Header::set(&mut data, &self.fields, name, value + blob.len() as i64);
            }
        }
        Header::set(&mut data, &self.fields, "NameCount", count + added);
        Header::set(
            &mut data,
            &self.fields,
            "NamesReferencedFromExportDataCount",
            at + added,
        );
        for generation in &self.generations {
            change_i32(&mut data, generation + 4, |count| count + added as i32);
        }
        data.splice(position..position, blob);
        self.data = data;
        self.reparse()
    }

    fn imports(&self, names: &[String]) -> Result<Vec<Import>> {
        let name = |at: usize| -> Result<String> {
            let (index, number) = (read_i32(&self.data, at), read_i32(&self.data, at + 4));
            let base = names
                .get(index as usize)
                .with_context(|| format!("name index {index} out of range"))?;
            Ok(if number != 0 {
                format!("{base}_{}", number - 1)
            } else {
                base.clone()
            })
        };
        (0..self.get("ImportCount") as usize)
            .map(|i| {
                let entry = self.get("ImportOffset") as usize + i * IMPORT_SIZE;
                Ok(Import {
                    class_package: name(entry)?,
                    class_name: name(entry + 8)?,
                    outer: read_i32(&self.data, entry + 16),
                    name: name(entry + 20)?,
                    flags: self.data[entry + 28..entry + 32].try_into().unwrap(),
                })
            })
            .collect()
    }

    /// Append import entries (32 bytes each, their names already in the name map).
    fn add_imports(self, entries: &[u8]) -> Result<Self> {
        let at = self.get("ImportOffset") as usize + self.get("ImportCount") as usize * IMPORT_SIZE;
        let count = self.get("ImportCount") + (entries.len() / IMPORT_SIZE) as i64;
        let mut header = self.insert_bytes(at, entries)?;
        Header::set(&mut header.data, &header.fields, "ImportCount", count);
        Ok(header)
    }

    /// Make the only export depend on `index` being created before it's serialized.
    fn add_create_before_serialization(self, index: i32) -> Result<Self> {
        if self.get("ExportCount") != 1 {
            bail!("expected one export");
        }
        let mut entry = self.export_offset(0) + 76; // FirstExportDependency, then the four counts
        let mut first = read_i32(&self.data, entry);
        let before_serialization = read_i32(&self.data, entry + 4);
        let create_before_serialization = read_i32(&self.data, entry + 8);
        if first < 0 {
            first = self.get("PreloadDependencyCount") as i32; // no dependencies yet
        }
        let at = self.get("PreloadDependencyOffset") as usize
            + 4 * (first + before_serialization + create_before_serialization) as usize;
        let count = self.get("PreloadDependencyCount");
        let mut header = self.insert_bytes(at, &index.to_le_bytes())?;
        if entry >= at {
            entry += 4;
        }
        write_i32(&mut header.data, entry, first);
        write_i32(&mut header.data, entry + 4, before_serialization);
        write_i32(&mut header.data, entry + 8, create_before_serialization + 1);
        Header::set(
            &mut header.data,
            &header.fields,
            "PreloadDependencyCount",
            count + 1,
        );
        header.reparse()
    }
}

// --- names and references ------------------------------------------------------------------------

/// An FName string -> (base, number) the way Unreal parses it: a trailing `_<digits>` (no leading
/// zero) is the number, stored plus one.
fn split_fname(name: &str) -> (&str, i32) {
    if let Some((base, number)) = name.rsplit_once('_') {
        let valid = !base.is_empty()
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
            && (number == "0" || !number.starts_with('0'));
        if valid {
            if let Ok(number) = number.parse::<i32>() {
                return (base, number + 1);
            }
        }
    }
    (name, 0)
}

/// An object reference as written in an edit: a path, or the *Copy Reference* form with its class,
/// `/Script/Engine.Texture2D'/Game/X/T_Icon.T_Icon'` (or `Texture2D'...'`) -> (class, path).
pub fn split_reference(reference: &str) -> (Option<&str>, &str) {
    if let Some(inner) = reference.strip_suffix('\'') {
        if let Some((class, path)) = inner.split_once('\'') {
            return (Some(class), path);
        }
    }
    (None, reference)
}

/// A new import's class when its field doesn't tell, from the shape of its object name.
const NEW_IMPORT_CLASSES: &[(&str, (&str, &str))] =
    &[("_C", ("/Script/Engine", "BlueprintGeneratedClass"))];

// --- encoding ------------------------------------------------------------------------------------

/// Numbers compared by value, so that `2` and `2.0` are the same; everything else exactly.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// A value's element `element` of a fixed-size array property, or the value itself.
fn field<'v>(object: &'v Value, property: &Property, element: usize) -> Option<&'v Value> {
    let value = object.get(&property.name)?;
    if property.array_dimension > 1 {
        value.get(element)
    } else {
        Some(value)
    }
}

struct Encoder<'a> {
    schemas: &'a Schemas,
    name_index: HashMap<String, i32>,
    objects: &'a HashMap<String, i32>,
    /// When collecting, names and objects the package lacks are gathered instead of failing.
    collect: bool,
    missing_names: Vec<String>,
    missing_objects: Vec<String>,
    /// The original data and where each original value was in it.
    original: &'a [u8],
    spans: &'a Spans,
    /// Copy values equal to the original instead of encoding them (off only to test the encoder).
    copy_unchanged: bool,
    out: Vec<u8>,
}

impl<'a> Encoder<'a> {
    fn i32(&mut self, value: i32) {
        self.out.extend(value.to_le_bytes());
    }

    fn fstring(&mut self, string: &str) {
        if string.is_empty() {
            self.i32(0);
        } else if string.is_ascii() {
            self.i32(string.len() as i32 + 1);
            self.out.extend(string.as_bytes());
            self.out.push(0);
        } else {
            let units: Vec<u16> = string.encode_utf16().chain([0]).collect();
            self.i32(-(units.len() as i32));
            for unit in units {
                self.out.extend(unit.to_le_bytes());
            }
        }
    }

    fn fname(&mut self, name: &str) -> Result<()> {
        if let Some(&index) = self.name_index.get(name) {
            self.i32(index);
            self.i32(0);
            return Ok(());
        }
        let (base, number) = split_fname(name);
        if let Some(&index) = self.name_index.get(base) {
            self.i32(index);
            self.i32(number);
            return Ok(());
        }
        if !self.collect {
            bail!("the name {name:?} isn't in the package's name map");
        }
        if !self.missing_names.iter().any(|missing| missing == base) {
            self.missing_names.push(base.to_string());
        }
        self.i32(0);
        self.i32(number);
        Ok(())
    }

    fn object_index(&mut self, value: &Value) -> Result<i32> {
        let Some(reference) = value.as_str() else {
            if value.is_null() {
                return Ok(0);
            }
            bail!("expected an object path, got {value}");
        };
        let path = split_reference(reference).1;
        if let Some(&index) = self.objects.get(path) {
            return Ok(index);
        }
        if !self.collect {
            bail!("the object {path} isn't imported by the package");
        }
        if !self.missing_objects.iter().any(|missing| missing == path) {
            self.missing_objects.push(path.to_string());
        }
        Ok(0)
    }

    fn enum_number(&self, property: &Property, value: &Value) -> Result<i64> {
        if let Some(number) = value.as_i64() {
            return Ok(number);
        }
        let name = value
            .as_str()
            .with_context(|| format!("expected an enum name, got {value}"))?;
        let values = property
            .enum_name
            .as_ref()
            .and_then(|enum_name| self.schemas.enums.get(enum_name));
        let found = values.and_then(|values| {
            values
                .iter()
                .find(|(_, enumerator)| enumerator.as_str() == name)
                .and_then(|(number, _)| number.parse().ok())
        });
        found.with_context(|| {
            format!(
                "{name:?} isn't a value of {}",
                property.enum_name.as_deref().unwrap_or("the enum")
            )
        })
    }

    fn is_zero(&self, property: &Property, value: &Value) -> Result<bool> {
        Ok(match property.type_name.as_str() {
            "Text" => value.as_str() == Some(""),
            "Enum" => self.enum_number(property, value)? == 0,
            "Byte" if property.enum_name.is_some() => self.enum_number(property, value)? == 0,
            _ => same(value, &zero_value(self.schemas, property)?),
        })
    }

    /// The original value at `path`, if it's equal to `value` and its bytes are known: copy them.
    fn copied(&mut self, path: &str, value: &Value, original: Option<&Value>) -> bool {
        if !self.copy_unchanged || original != Some(value) {
            return false;
        }
        match self.spans.get(path) {
            Some(&(start, end)) => {
                self.out.extend_from_slice(&self.original[start..end]);
                true
            }
            None => false,
        }
    }

    fn header(&mut self, entries: &[(usize, bool)]) {
        // Fragments of consecutive properties; one can skip at most 127 and hold at most 127
        let mut fragments: Vec<(usize, Vec<bool>)> = vec![];
        let (mut previous, mut i) = (0, 0);
        while i < entries.len() {
            let mut skip = entries[i].0 - previous;
            while skip > 127 {
                fragments.push((127, vec![]));
                skip -= 127;
            }
            let mut run = vec![entries[i]];
            while i + 1 < entries.len()
                && entries[i + 1].0 == run.last().unwrap().0 + 1
                && run.len() < 127
            {
                i += 1;
                run.push(entries[i]);
            }
            previous = run.last().unwrap().0 + 1;
            fragments.push((skip, run.into_iter().map(|(_, zero)| zero).collect()));
            i += 1;
        }
        if fragments.is_empty() {
            fragments.push((0, vec![]));
        }
        let last = fragments.len() - 1;
        for (n, (skip, zeros)) in fragments.iter().enumerate() {
            let packed = *skip as u16
                | if zeros.iter().any(|&zero| zero) {
                    0x80
                } else {
                    0
                }
                | if n == last { 0x100 } else { 0 }
                | (zeros.len() as u16) << 9;
            self.out.extend(packed.to_le_bytes());
        }
        let zeros: Vec<bool> = fragments.into_iter().flat_map(|(_, zeros)| zeros).collect();
        if zeros.iter().any(|&zero| zero) {
            let bytes = match zeros.len() {
                0..=8 => 1,
                9..=16 => 2,
                total => total.div_ceil(32) * 4,
            };
            let mut mask = vec![0u8; bytes];
            for (bit, _) in zeros.iter().enumerate().filter(|(_, &zero)| zero) {
                mask[bit / 8] |= 1 << (bit % 8);
            }
            self.out.extend(mask);
        }
    }

    fn struct_value(
        &mut self,
        name: &str,
        value: &Value,
        path: &str,
        original: Option<&Value>,
    ) -> Result<()> {
        if name == "GameplayTagContainer" {
            let tags = value
                .as_array()
                .context("expected a list of gameplay tags")?;
            self.i32(tags.len() as i32);
            for tag in tags {
                self.fname(tag.as_str().context("expected a gameplay tag name")?)?;
            }
            return Ok(());
        }
        let properties = self.schemas.all_properties(name)?;
        if NATIVE_STRUCTS.contains(&name) {
            for (property, element) in properties {
                let key = format!("{path}/{}", property_key(property, element));
                let field_value = field(value, property, element)
                    .filter(|value| !value.is_null())
                    .with_context(|| format!("{name}.{} is missing", property.name))?;
                let original_field =
                    original.and_then(|original| field(original, property, element));
                self.value(property, field_value, &key, original_field)?;
            }
            return Ok(());
        }
        let mut entries = vec![];
        let mut values = vec![];
        for (index, &(property, element)) in properties.iter().enumerate() {
            let key = format!("{path}/{}", property_key(property, element));
            let field_value = field(value, property, element).unwrap_or(&Value::Null);
            let original_field = original.and_then(|original| field(original, property, element));
            let (present, zero) = if original.is_some() && original_field == Some(field_value) {
                // unchanged: as the cooker wrote it (absent, marked zero, or stored)
                match self.spans.get(&key) {
                    Some(&(start, end)) => (true, start == end),
                    None => (false, false),
                }
            } else if field_value.is_null() {
                (false, false)
            } else {
                (true, self.is_zero(property, field_value)?)
            };
            if present {
                entries.push((index, zero));
                if !zero {
                    values.push((property, field_value, key, original_field));
                }
            }
        }
        self.header(&entries);
        for (property, field_value, key, original_field) in values {
            self.value(property, field_value, &key, original_field)
                .with_context(|| format!("{name}.{}", property.name))?;
        }
        Ok(())
    }

    fn text(&mut self, value: &Value) -> Result<()> {
        let flags = value.get("flags").and_then(Value::as_u64).unwrap_or(0);
        self.i32(flags as i32);
        let plain = match value {
            Value::String(string) => Some(string.as_str()),
            Value::Object(object) if object.contains_key("string") => object["string"].as_str(),
            _ => None,
        };
        match value {
            _ if plain.is_some() => {
                let string = plain.unwrap();
                self.out.push(-1i8 as u8); // history None: the same in every language
                self.i32(if string.is_empty() { 0 } else { 1 });
                if !string.is_empty() {
                    self.fstring(string);
                }
            }
            Value::Object(object) if object.contains_key("table") => {
                self.out.push(11); // a string table entry
                self.fname(object["table"].as_str().context("text table")?)?;
                self.fstring(
                    object
                        .get("key")
                        .and_then(Value::as_str)
                        .context("text key")?,
                );
            }
            Value::Object(object) => {
                self.out.push(0); // history Base: namespace, key, source
                for part in ["namespace", "key", "source"] {
                    let text = object.get(part).and_then(Value::as_str);
                    let text = text.with_context(|| format!("text {part}"))?;
                    if part == "namespace" && text.is_empty() {
                        // the cooker writes an empty namespace as just its terminator
                        self.i32(1);
                        self.out.push(0);
                    } else {
                        self.fstring(text);
                    }
                }
            }
            other => bail!("expected text, got {other}"),
        }
        Ok(())
    }

    fn integer(value: &Value) -> Result<i64> {
        match value.as_i64() {
            Some(number) => Ok(number),
            None => match value.as_f64() {
                Some(number) if number.fract() == 0.0 => Ok(number as i64),
                _ => bail!("expected a whole number, got {value}"),
            },
        }
    }

    fn value(
        &mut self,
        property: &Property,
        value: &Value,
        path: &str,
        original: Option<&Value>,
    ) -> Result<()> {
        if self.copied(path, value, original) {
            return Ok(());
        }
        let number = |value: &Value| {
            value
                .as_f64()
                .with_context(|| format!("expected a number, got {value}"))
        };
        match property.type_name.as_str() {
            "Int8" => self.out.push(Self::integer(value)? as i8 as u8),
            "Int16" => self
                .out
                .extend((Self::integer(value)? as i16).to_le_bytes()),
            "Int" => self.i32(Self::integer(value)? as i32),
            "Int64" => self.out.extend(Self::integer(value)?.to_le_bytes()),
            "UInt16" => self
                .out
                .extend((Self::integer(value)? as u16).to_le_bytes()),
            "UInt32" => self
                .out
                .extend((Self::integer(value)? as u32).to_le_bytes()),
            "UInt64" => {
                let unsigned = value
                    .as_u64()
                    .with_context(|| format!("expected a whole number, got {value}"))?;
                self.out.extend(unsigned.to_le_bytes());
            }
            "Float" => self.out.extend((number(value)? as f32).to_le_bytes()),
            "Double" | "LargeWorldCoordinatesReal" => self.out.extend(number(value)?.to_le_bytes()),
            "Byte" => {
                let byte = if property.enum_name.is_some() {
                    self.enum_number(property, value)?
                } else {
                    Self::integer(value)?
                };
                self.out.push(byte as u8);
            }
            "Enum" => {
                let number = self.enum_number(property, value)?;
                match property.underlying.as_deref().unwrap_or("Byte") {
                    "Byte" => self.out.push(number as u8),
                    "Int" => self.i32(number as i32),
                    "UInt16" => self.out.extend((number as u16).to_le_bytes()),
                    "UInt32" => self.out.extend((number as u32).to_le_bytes()),
                    "Int64" => self.out.extend(number.to_le_bytes()),
                    underlying => bail!("unsupported enum underlying type {underlying}"),
                }
            }
            "Bool" => self.out.push(
                value
                    .as_bool()
                    .with_context(|| format!("expected true or false, got {value}"))?
                    as u8,
            ),
            "Name" => self.fname(
                value
                    .as_str()
                    .with_context(|| format!("expected a name, got {value}"))?,
            )?,
            "Str" => self.fstring(
                value
                    .as_str()
                    .with_context(|| format!("expected a string, got {value}"))?,
            ),
            "Text" => self.text(value)?,
            "Object" | "Class" | "Interface" | "WeakObject" | "LazyObject" => {
                let index = self.object_index(value)?;
                self.i32(index);
            }
            "SoftObject" | "SoftClass" => {
                let (package, asset, sub_path) = match value.as_str() {
                    None => ("None", "None", ""),
                    Some(path) => {
                        let (object, sub_path) = path.split_once(':').unwrap_or((path, ""));
                        let (package, asset) = object.split_once('.').unwrap_or((object, "None"));
                        (package, asset, sub_path)
                    }
                };
                self.fname(package)?;
                self.fname(asset)?;
                self.fstring(sub_path);
            }
            "Struct" => {
                let name = property.struct_name.as_deref().unwrap_or_default();
                self.struct_value(name, value, path, original)?;
            }
            "Array" | "Set" => {
                if property.type_name == "Set" {
                    self.i32(0); // elements to remove
                }
                let inner = property
                    .inner
                    .as_deref()
                    .context("container without inner property")?;
                let elements = value
                    .as_array()
                    .with_context(|| format!("expected a list, got {value}"))?;
                self.i32(elements.len() as i32);
                for (i, element) in elements.iter().enumerate() {
                    let key = format!("{path}/{i}");
                    let original_element = original.and_then(|original| original.get(i));
                    if self.copied(&key, element, original_element) {
                        continue;
                    }
                    self.element(inner, element, &key, original_element)?;
                }
            }
            "Map" => {
                self.i32(0); // keys to remove
                let key_property = property.key.as_deref().context("map without key")?;
                let value_property = property.value.as_deref().context("map without value")?;
                let entries = value
                    .as_object()
                    .with_context(|| format!("expected an object, got {value}"))?;
                self.i32(entries.len() as i32);
                for (key, entry) in entries {
                    // decoded keys are strings, or JSON text for other types
                    let key_value = match key_property.type_name.as_str() {
                        "Name" | "Str" | "Enum" | "Text" => Value::String(key.clone()),
                        _ => serde_json::from_str(key).with_context(|| format!("map key {key}"))?,
                    };
                    self.element(key_property, &key_value, "", None)?;
                    self.element(value_property, entry, "", None)?;
                }
            }
            type_name => bail!("writing {type_name} properties isn't supported"),
        }
        Ok(())
    }

    /// Container elements: enums are stored as FNames (`EType::Value`) rather than numbers.
    fn element(
        &mut self,
        property: &Property,
        value: &Value,
        path: &str,
        original: Option<&Value>,
    ) -> Result<()> {
        if property.type_name == "Enum" {
            let name = match value {
                Value::String(name) => name.clone(),
                number => {
                    match enum_name(self.schemas, property, self.enum_number(property, number)?) {
                        Value::String(name) => name,
                        _ => bail!(
                            "{number} has no name in {:?}, so it can't be a list element",
                            property.enum_name
                        ),
                    }
                }
            };
            let enum_type = property.enum_name.as_deref().unwrap_or_default();
            return self.fname(&format!("{enum_type}::{name}"));
        }
        self.value(property, value, path, original)
    }
}

// --- the table -----------------------------------------------------------------------------------

/// Every Object/Class value in rows of `row_struct`: (field, value), the field as its property
/// names joined by `.`, with `[]` for list elements (e.g. `DisplayIcon`, `CustomerClasses[]`).
fn object_fields(
    schemas: &Schemas,
    row_struct: &str,
    rows: &[&Value],
) -> Result<Vec<(String, String)>> {
    fn walk(
        schemas: &Schemas,
        property: &Property,
        value: &Value,
        field: &str,
        out: &mut Vec<(String, String)>,
    ) -> Result<()> {
        match property.type_name.as_str() {
            "Object" | "Class" => {
                if let Some(path) = value.as_str() {
                    out.push((field.to_string(), path.to_string()));
                }
            }
            "Struct" => {
                let name = property.struct_name.as_deref().unwrap_or_default();
                if name != "GameplayTagContainer" && value.is_object() {
                    walk_struct(schemas, name, value, field, out)?;
                }
            }
            "Array" | "Set" => {
                if let (Some(inner), Some(elements)) = (property.inner.as_deref(), value.as_array())
                {
                    for element in elements {
                        walk(schemas, inner, element, &format!("{field}[]"), out)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn walk_struct(
        schemas: &Schemas,
        name: &str,
        value: &Value,
        field: &str,
        out: &mut Vec<(String, String)>,
    ) -> Result<()> {
        for (property, element) in schemas.all_properties(name)? {
            if let Some(field_value) = self::field(value, property, element) {
                let field = if field.is_empty() {
                    property.name.clone()
                } else {
                    format!("{field}.{}", property.name)
                };
                walk(schemas, property, field_value, &field, out)?;
            }
        }
        Ok(())
    }
    let mut out = vec![];
    for row in rows {
        walk_struct(schemas, row_struct, row, "", &mut out)?;
    }
    Ok(out)
}

/// A written table.
pub struct Written {
    pub uasset: Vec<u8>,
    pub uexp: Vec<u8>,
    /// The references it didn't have before: (object path, class path), e.g.
    /// (`/Game/Mods/X/T_Icon.T_Icon`, `/Script/Engine.Texture2D`).
    pub references: Vec<(String, String)>,
}

/// A DataTable decoded for editing: change [`Table::rows`] (or add rows with [`Table::add_row`]),
/// then [`Table::save`].
pub struct Table<'a> {
    schemas: &'a Schemas,
    uasset: Vec<u8>,
    uexp: Vec<u8>,
    package: Package,
    pub row_struct: String,
    /// Where the row count is in the `.uexp`, and where the data after the rows starts.
    rows_start: usize,
    tail_start: usize,
    spans: Spans,
    /// The rows as decoded (texts as their keys, not translated), with each row's span.
    original: IndexMap<String, (Value, (usize, usize))>,
    pub rows: IndexMap<String, Value>,
    /// New row -> the existing row whose layout it follows (see [`Table::add_row`]).
    layouts: HashMap<String, String>,
}

impl<'a> Table<'a> {
    pub fn open(uasset: &[u8], uexp: &[u8], schemas: &'a Schemas) -> Result<Self> {
        let package = Package::read(uasset)?;
        let (row_struct, rows_start, tail_start, spans, original) = {
            let mut decoder = Decoder::new(uexp, &package, schemas, None);
            decoder.spans = Some(Spans::new());
            let class = main_class(&package)?;
            if class != "DataTable" {
                bail!("only plain DataTables can be written, not a {class}");
            }
            let properties = decoder.object(class)?;
            let row_struct_path = properties["RowStruct"]
                .as_str()
                .context("DataTable has no RowStruct")?;
            let row_struct = row_struct_path.rsplit('.').next().unwrap().to_string();
            let rows_start = decoder.reader.position;
            let mut original = IndexMap::new();
            for _ in 0..decoder.reader.i32()? {
                let name = decoder.reader.fname()?;
                let start = decoder.reader.position;
                decoder.path = vec![name.clone()];
                let row = decoder
                    .struct_value(&row_struct)
                    .with_context(|| format!("row {name}"))?;
                original.insert(name, (row, (start, decoder.reader.position)));
            }
            let tail_start = decoder.reader.position;
            (
                row_struct,
                rows_start,
                tail_start,
                decoder.spans.take().unwrap(),
                original,
            )
        };
        Ok(Table {
            schemas,
            uasset: uasset.to_vec(),
            uexp: uexp.to_vec(),
            package,
            row_struct,
            rows_start,
            tail_start,
            spans,
            rows: original
                .iter()
                .map(|(name, (row, _))| (name.clone(), row.clone()))
                .collect(),
            original,
            layouts: HashMap::new(),
        })
    }

    /// Open a table from converted game assets, by its asset path.
    pub fn open_asset(
        assets: &crate::unpack::Assets,
        path: &str,
        schemas: &'a Schemas,
    ) -> Result<Self> {
        let (uasset, uexp) = assets.asset(path)?;
        Table::open(uasset, uexp, schemas)
    }

    /// Add (or replace) a row. `like` is an existing row whose layout it follows where its values
    /// are the same: a row copied from it keeps its exact bytes for everything not changed.
    pub fn add_row(&mut self, name: &str, row: Value, like: &str) -> Result<()> {
        if !self.original.contains_key(like) {
            bail!("no row {like} to follow");
        }
        self.rows.insert(name.to_string(), row);
        if !self.original.contains_key(name) {
            self.layouts.insert(name.to_string(), like.to_string());
        }
        Ok(())
    }

    /// The rows that differ from the file (new rows included).
    pub fn changed_rows(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|(name, row)| {
                self.original
                    .get(*name)
                    .is_none_or(|(original, _)| original != *row)
            })
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// The row whose layout and bytes a row is encoded against.
    fn layout_of<'s>(&'s self, name: &'s str) -> &'s str {
        self.layouts.get(name).map(String::as_str).unwrap_or(name)
    }

    fn encoder<'s>(
        &'s self,
        names: &[String],
        objects: &'s HashMap<String, i32>,
        collect: bool,
    ) -> Encoder<'s> {
        Encoder {
            schemas: self.schemas,
            name_index: names
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), i as i32))
                .collect(),
            objects,
            collect,
            missing_names: vec![],
            missing_objects: vec![],
            original: &self.uexp,
            spans: &self.spans,
            copy_unchanged: true,
            out: vec![],
        }
    }

    fn encode_row(&self, encoder: &mut Encoder, name: &str) -> Result<()> {
        let layout = self.layout_of(name);
        let original = self.original.get(layout).map(|(row, _)| row);
        encoder
            .struct_value(&self.row_struct, &self.rows[name], layout, original)
            .with_context(|| format!("row {name}"))
    }

    /// The classes of the new references in the changed rows (see the module documentation).
    fn new_reference_classes(
        &self,
        changed: &[&str],
        imports: &[Import],
        objects: &HashMap<String, i32>,
    ) -> Result<HashMap<String, (String, String)>> {
        let originals: Vec<&Value> = self.original.values().map(|(row, _)| row).collect();
        let mut by_field: HashMap<String, HashSet<(String, String)>> = HashMap::new();
        for (field, path) in object_fields(self.schemas, &self.row_struct, &originals)? {
            if let Some(&index) = objects.get(&path).filter(|&&index| index < 0) {
                let import = &imports[(-index - 1) as usize];
                by_field
                    .entry(field)
                    .or_default()
                    .insert((import.class_package.clone(), import.class_name.clone()));
            }
        }
        let rows: Vec<&Value> = changed.iter().map(|name| &self.rows[*name]).collect();
        let mut out = HashMap::new();
        for (field, reference) in object_fields(self.schemas, &self.row_struct, &rows)? {
            let (class, path) = split_reference(&reference);
            if objects.contains_key(path) {
                continue;
            }
            let expected = by_field.get(&field);
            let found = if let Some(class) = class {
                let (package, name) = match class.rsplit_once('.') {
                    Some((package, name)) => (package.to_string(), name.to_string()),
                    // Texture2D'...': the module of a class the table already uses, else Engine
                    None => {
                        let module = imports
                            .iter()
                            .find(|import| {
                                import.class_name == class
                                    && import.class_package.starts_with("/Script/")
                            })
                            .map(|import| import.class_package.clone());
                        (
                            module.unwrap_or_else(|| "/Script/Engine".to_string()),
                            class.to_string(),
                        )
                    }
                };
                if let Some(expected) =
                    expected.filter(|expected| !expected.contains(&(package.clone(), name.clone())))
                {
                    let mut holds: Vec<&str> =
                        expected.iter().map(|(_, name)| name.as_str()).collect();
                    holds.sort();
                    bail!(
                        "{field}: {path} is given as a {name}, but the field holds {}",
                        holds.join(", ")
                    );
                }
                (package, name)
            } else if let Some(only) = expected.filter(|expected| expected.len() == 1) {
                only.iter().next().unwrap().clone()
            } else {
                let object = path.rsplit('.').next().unwrap_or(path);
                match NEW_IMPORT_CLASSES
                    .iter()
                    .find(|(suffix, _)| object.ends_with(suffix))
                {
                    Some((_, (package, name))) => (package.to_string(), name.to_string()),
                    None => bail!(
                        "{field}: can't tell the class of {path}; write it with its class, as the \
                         editor's Copy Reference does, e.g. /Script/Engine.Texture2D'{path}'"
                    ),
                }
            };
            if let Some(previous) = out.insert(path.to_string(), found.clone()) {
                if previous != found {
                    bail!("{path} is used as both a {} and a {}", previous.1, found.1);
                }
            }
        }
        Ok(out)
    }

    /// The table's `.uasset` and `.uexp`, with the changed rows re-encoded.
    pub fn save(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        let written = self.write()?;
        Ok((written.uasset, written.uexp))
    }

    /// Like [`Table::save`], also listing the references the table didn't have before.
    pub fn write(&self) -> Result<Written> {
        self.save_with(true)
    }

    fn save_with(&self, copy_unchanged: bool) -> Result<Written> {
        let changed: Vec<&str> = if copy_unchanged {
            self.changed_rows()
        } else {
            self.rows.keys().map(String::as_str).collect()
        };
        let mut names = self.package.names.clone();
        let mut header = Header::parse(self.uasset.clone())?;
        let imports = header.imports(&names)?;
        let mut objects: HashMap<String, i32> = HashMap::new();
        for (i, path) in self.package.import_paths.iter().enumerate() {
            objects.entry(path.clone()).or_insert(-(i as i32 + 1));
        }
        for (i, path) in self.package.export_paths.iter().enumerate() {
            objects.entry(path.clone()).or_insert(i as i32 + 1);
        }

        // Pass 1: the names and imports the changed rows need that the package lacks
        let (mut missing_names, missing_objects) = {
            let mut encoder = self.encoder(&names, &objects, true);
            encoder.copy_unchanged = copy_unchanged;
            for name in &changed {
                encoder.fname(name)?;
                self.encode_row(&mut encoder, name)?;
            }
            (encoder.missing_names, encoder.missing_objects)
        };
        let classes = self.new_reference_classes(&changed, &imports, &objects)?;
        let mut records: Vec<Import> = vec![];
        for path in &missing_objects {
            let (package, object) = path.split_once('.').unwrap_or((path, ""));
            if !package.starts_with('/') || object.is_empty() || object.contains(['.', ':']) {
                bail!(
                    "can't add a reference to {path:?}: give an asset as \
                     /Game/<folders>/<asset>.<asset> \
                     (a Blueprint class as /Game/<folders>/BP_X.BP_X_C)"
                );
            }
            if !objects.contains_key(package) {
                records.push(Import {
                    class_package: "/Script/CoreUObject".into(),
                    class_name: "Package".into(),
                    outer: 0,
                    name: package.into(),
                    flags: [0; 4],
                });
                objects.insert(
                    package.to_string(),
                    -((imports.len() + records.len()) as i32),
                );
            }
            let (class_package, class_name) = classes
                .get(path)
                .with_context(|| format!("no class for the new reference {path}"))?
                .clone();
            records.push(Import {
                class_package,
                class_name,
                outer: objects[package],
                name: object.into(),
                flags: [0; 4],
            });
            objects.insert(path.clone(), -((imports.len() + records.len()) as i32));
        }
        for record in &mut records {
            // the last field (flags) as on an existing import of the same class
            if let Some(same) = imports.iter().find(|import| {
                (&import.class_package, &import.class_name)
                    == (&record.class_package, &record.class_name)
            }) {
                record.flags = same.flags;
            }
            for name in [&record.class_package, &record.class_name, &record.name] {
                let base = split_fname(name).0;
                if !names.iter().any(|existing| existing == base)
                    && !missing_names.iter().any(|missing| missing == base)
                {
                    missing_names.push(base.to_string());
                }
            }
        }
        if !missing_names.is_empty() {
            let at = header.get("NamesReferencedFromExportDataCount") as usize;
            header = header.insert_names(&missing_names)?;
            names.splice(at..at, missing_names.iter().cloned());
        }
        if !records.is_empty() {
            let mut encoder = self.encoder(&names, &objects, false);
            for record in &records {
                encoder.fname(&record.class_package)?;
                encoder.fname(&record.class_name)?;
                encoder.i32(record.outer);
                encoder.fname(&record.name)?;
                encoder.out.extend(record.flags);
            }
            header = header.add_imports(&encoder.out)?;
            for path in &missing_objects {
                // the table can only be loaded once the referenced objects exist
                header = header.add_create_before_serialization(objects[path])?;
            }
        }

        // Pass 2: the .uexp, with every name and import in place
        let mut encoder = self.encoder(&names, &objects, false);
        encoder.copy_unchanged = copy_unchanged;
        encoder.out.extend_from_slice(&self.uexp[..self.rows_start]);
        encoder.i32(self.rows.len() as i32);
        for name in self.rows.keys() {
            encoder.fname(name)?;
            match self.original.get(name) {
                Some((row, (start, end))) if copy_unchanged && row == &self.rows[name] => {
                    encoder.out.extend_from_slice(&self.uexp[*start..*end]);
                }
                _ => self.encode_row(&mut encoder, name)?,
            }
        }
        encoder.out.extend_from_slice(&self.uexp[self.tail_start..]);
        let uexp = encoder.out;

        // The export's size (it excludes the 4-byte package tag), and where bulk data would start
        if header.get("ExportCount") != 1 {
            bail!("expected one export");
        }
        let (old_size, new_size) = (self.uexp.len() as i64 - 4, uexp.len() as i64 - 4);
        let size_at = header.export_offset(0) + 28;
        if read_i64(&header.data, size_at) != old_size {
            bail!("the export's SerialSize doesn't match the .uexp");
        }
        write_i64(&mut header.data, size_at, new_size);
        let bulk = header.get("BulkDataStartOffset");
        if bulk > 0 {
            Header::set(
                &mut header.data,
                &header.fields,
                "BulkDataStartOffset",
                bulk + new_size - old_size,
            );
        }
        let references = missing_objects
            .iter()
            .map(|path| {
                let (package, name) = &classes[path];
                (path.clone(), format!("{package}.{name}"))
            })
            .collect();
        Ok(Written {
            uasset: header.data,
            uexp,
            references,
        })
    }

    /// Re-encode every row from its decoded values (copying nothing): the result must be the
    /// original bytes, which tests the encoder against the game's own files.
    pub fn reencode_all(&self) -> Result<Vec<u8>> {
        Ok(self.save_with(false)?.uexp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fname_numbers() {
        assert_eq!(split_fname("NewRow_0"), ("NewRow", 1));
        assert_eq!(split_fname("Name_12"), ("Name", 13));
        assert_eq!(split_fname("Name_012"), ("Name_012", 0));
        assert_eq!(split_fname("Plain"), ("Plain", 0));
        assert_eq!(split_fname("_3"), ("_3", 0));
    }

    #[test]
    fn references() {
        assert_eq!(
            split_reference("/Script/Engine.Texture2D'/Game/X/T.T'"),
            (Some("/Script/Engine.Texture2D"), "/Game/X/T.T")
        );
        assert_eq!(
            split_reference("/Game/X/BP.BP_C"),
            (None, "/Game/X/BP.BP_C")
        );
    }

    #[test]
    fn numbers_compare_by_value() {
        assert!(same(&serde_json::json!(2), &serde_json::json!(2.0)));
        assert!(!same(&serde_json::json!(2), &serde_json::json!(2.5)));
    }
}
