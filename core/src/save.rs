//! Alchemy Factory save files (`.sav`), and edits to the template save new games start from.
//!
//! A save is the magic `\xd5CHAF`, a small list of tagged properties (IsSandbox, Day, Level, Gold,
//! ..., Name) ending in `None`, then the body: the tag `b7 cf d5 50` and a version byte, then
//! Oodle-compressed blocks `{u32 compressed size, u64 size, u64 compressed size, data}`. Version 2
//! (the template, and saves before the game's 2026-10-16 update) has the blocks right after the
//! version byte; version 3 has 8 more bytes first (`01 00 00 00`, then 4 bytes that differ per
//! save, probably a checksum).
//!
//! The body holds the game's objects in a custom framing, but their values are UE 5.4+ tagged
//! properties (name, type name, size, flags, value), so a property is found by its tag. Edits keep
//! every value's size, so the framing (sizes and offset tables, not yet understood) stays valid:
//! numbers and true/false can change, names, strings and list lengths can't yet.

use anyhow::{bail, Context, Result};
use oodle_loader::{CompressionLevel, Compressor};
use serde_json::{json, Value};

use crate::reader::Reader;

const MAGIC: &[u8] = b"\xd5CHAF";
const BODY_TAG: &[u8] = &[0xb7, 0xcf, 0xd5, 0x50];

/// The template save new games start from, in the game's `.pak`.
pub const TEMPLATE: &str = "AlchemyFactory/Content/PreviewSaveGames/DefaultGame.sav";

/// A property tag: (name, type name, size, flags).
struct Tag {
    type_name: String,
    type_parameters: Vec<String>,
    size: i32,
    /// Position of the flags byte (true/false values are a flag), and of the value.
    flags_position: usize,
    value_position: usize,
}

/// FPropertyTypeName: a name and its parameters, e.g. (ArrayProperty, [NameProperty]); only the
/// parameters' own names are kept.
fn type_name(reader: &mut Reader) -> Result<(String, Vec<String>)> {
    let name = reader.fstring()?;
    let count = reader.i32()?;
    if !(0..16).contains(&count) {
        bail!("bad type parameter count {count}");
    }
    let mut parameters = vec![];
    for _ in 0..count {
        parameters.push(type_name(reader)?.0);
    }
    Ok((name, parameters))
}

/// The tag at `position`, or None at the `None` that ends a property list.
fn tag(data: &[u8], position: usize) -> Result<Option<(String, Tag)>> {
    let mut reader = Reader::new(data, &[]);
    reader.position = position;
    let name = reader.fstring()?;
    if name.is_empty() || name == "None" {
        return Ok(None);
    }
    let (type_name, type_parameters) = type_name(&mut reader)?;
    let size = reader.i32()?;
    let flags_position = reader.position;
    let flags = reader.u8()?;
    if flags & 0x01 != 0 {
        reader.i32()?; // array index
    }
    if flags & 0x02 != 0 {
        reader.take(16)?; // property GUID
    }
    if flags & 0x04 != 0 {
        bail!("property extensions aren't supported");
    }
    let value_position = reader.position;
    Ok(Some((
        name,
        Tag {
            type_name,
            type_parameters,
            size,
            flags_position,
            value_position,
        },
    )))
}

/// The header's simple properties (numbers, true/false, strings), and where the body starts.
pub fn read_header(data: &[u8]) -> Result<(serde_json::Map<String, Value>, usize)> {
    if !data.starts_with(MAGIC) {
        bail!("not an Alchemy Factory save (bad magic)");
    }
    let first = find(data, b"IsSandbox\0", 0).context("no header properties")? - 4;
    let mut out = serde_json::Map::new();
    let mut position = first;
    while let Some((name, tag)) = tag(data, position)? {
        let value = fixed_value(data, &tag).ok().or_else(|| {
            (tag.type_name == "StrProperty").then(|| {
                let mut reader = Reader::new(data, &[]);
                reader.position = tag.value_position;
                reader.fstring().map(Value::String).unwrap_or(Value::Null)
            })
        });
        if let Some(value) = value {
            out.insert(name, value);
        }
        position = tag.value_position + tag.size as usize;
    }
    // past the terminating "None"
    let mut reader = Reader::new(data, &[]);
    reader.position = position;
    reader.fstring()?;
    Ok((out, reader.position))
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Where the body's blocks start.
fn blocks_start(data: &[u8], offset: usize) -> Result<usize> {
    if data.get(offset..offset + 4) != Some(BODY_TAG) {
        bail!("save body not found at {offset}");
    }
    match data[offset + 4] {
        2 => Ok(offset + 5),
        3 => Ok(offset + 13),
        version => bail!("unknown save body version {version}"),
    }
}

/// The body's blocks: (uncompressed size, compressed data).
fn blocks(data: &[u8], offset: usize) -> Result<Vec<(usize, &[u8])>> {
    let mut out = vec![];
    let mut position = blocks_start(data, offset)?;
    while position < data.len() {
        let mut reader = Reader::new(data, &[]);
        reader.position = position;
        let (compressed, size, compressed_again) = (reader.u32()?, reader.u64()?, reader.u64()?);
        if compressed as u64 != compressed_again {
            bail!("inconsistent block header at {position}");
        }
        position = reader.position;
        out.push((size as usize, reader.take(compressed as usize)?));
        position += compressed as usize;
    }
    Ok(out)
}

/// The decompressed body.
pub fn decompress_body(data: &[u8], offset: usize) -> Result<Vec<u8>> {
    let oodle = oodle_loader::oodle().map_err(|error| anyhow::anyhow!("loading Oodle: {error}"))?;
    let mut body = vec![];
    for (size, compressed) in blocks(data, offset)? {
        let mut block = vec![0; size];
        if oodle.decompress(compressed, &mut block) != size as isize {
            bail!("Oodle decompression failed");
        }
        body.extend(block);
    }
    Ok(body)
}

/// A save's header properties and decompressed body.
pub fn read_save(data: &[u8]) -> Result<(serde_json::Map<String, Value>, Vec<u8>)> {
    let (header, offset) = read_header(data)?;
    Ok((header, decompress_body(data, offset)?))
}

/// The save with its body replaced by `body`, in blocks of the same sizes (the last takes the
/// rest). Only version-2 bodies (the template) can be written; version 3 has a checksum.
pub fn write_body(data: &[u8], offset: usize, body: &[u8]) -> Result<Vec<u8>> {
    if blocks_start(data, offset)? != offset + 5 {
        bail!(
            "only version-2 save bodies (the template save) can be written; version 3 has a \
            checksum"
        );
    }
    let sizes: Vec<usize> = blocks(data, offset)?
        .into_iter()
        .map(|(size, _)| size)
        .collect();
    let oodle = oodle_loader::oodle().map_err(|error| anyhow::anyhow!("loading Oodle: {error}"))?;
    let mut out = data[..offset + 5].to_vec();
    let mut position = 0;
    for (i, size) in sizes.iter().enumerate() {
        let end = if i == sizes.len() - 1 {
            body.len()
        } else {
            (position + size).min(body.len())
        };
        let chunk = &body[position..end];
        let packed = oodle
            .compress(chunk, Compressor::Kraken, CompressionLevel::Normal)
            .map_err(|error| anyhow::anyhow!("Oodle compression failed: {error}"))?;
        out.extend((packed.len() as u32).to_le_bytes());
        out.extend((chunk.len() as u64).to_le_bytes());
        out.extend((packed.len() as u64).to_le_bytes());
        out.extend(packed);
        position = end;
    }
    Ok(out)
}

/// The positions of the tags named `name`: an FString of the name, then a type name ending in
/// `Property` (names also appear in name tables, which this skips).
fn tag_positions(data: &[u8], name: &str, from: usize) -> Vec<usize> {
    let mut needle = ((name.len() + 1) as i32).to_le_bytes().to_vec();
    needle.extend(name.as_bytes());
    needle.push(0);
    let mut out = vec![];
    let mut at = from;
    while let Some(found) = find(data, &needle, at) {
        let type_at = found + needle.len();
        let is_tag = data.get(type_at..type_at + 4).is_some_and(|length| {
            let length = i32::from_le_bytes(length.try_into().unwrap());
            (10..64).contains(&length)
                && data
                    .get(type_at + 4..type_at + 4 + length as usize)
                    .is_some_and(|type_name| {
                        type_name.ends_with(b"Property\0")
                            && type_name[..type_name.len() - 1]
                                .iter()
                                .all(u8::is_ascii_alphanumeric)
                    })
        });
        if is_tag {
            out.push(found);
        }
        at = found + 1;
    }
    out
}

/// The tag of a property: the `occurrence`-th one called `name`, counting from the first tag called
/// `after` if given.
fn locate(data: &[u8], name: &str, occurrence: usize, after: Option<&str>) -> Result<Tag> {
    let from = match after {
        Some(after) => {
            *tag_positions(data, after, 0)
                .first()
                .with_context(|| format!("no property {after}"))?
                + 1
        }
        None => 0,
    };
    let position = tag_positions(data, name, from)
        .get(occurrence)
        .copied()
        .with_context(|| {
            format!(
                "no property {name}{}{}",
                if occurrence > 0 {
                    format!(" number {}", occurrence + 1)
                } else {
                    String::new()
                },
                after
                    .map(|after| format!(" after {after}"))
                    .unwrap_or_default()
            )
        })?;
    Ok(tag(data, position)?.context("not a property tag")?.1)
}

/// The value of a number or true/false tag.
fn fixed_value(data: &[u8], tag: &Tag) -> Result<Value> {
    let at = tag.value_position;
    let bytes = |count: usize| data.get(at..at + count).context("value past the end");
    Ok(match tag.type_name.as_str() {
        "BoolProperty" => json!(data[tag.flags_position] & 0x10 != 0),
        "IntProperty" => json!(i32::from_le_bytes(bytes(4)?.try_into()?)),
        "UInt32Property" => json!(u32::from_le_bytes(bytes(4)?.try_into()?)),
        "Int64Property" => json!(i64::from_le_bytes(bytes(8)?.try_into()?)),
        "UInt64Property" => json!(u64::from_le_bytes(bytes(8)?.try_into()?)),
        "Int8Property" => json!(bytes(1)?[0] as i8),
        "Int16Property" => json!(i16::from_le_bytes(bytes(2)?.try_into()?)),
        "UInt16Property" => json!(u16::from_le_bytes(bytes(2)?.try_into()?)),
        "FloatProperty" => json!(f32::from_le_bytes(bytes(4)?.try_into()?)),
        "DoubleProperty" => json!(f64::from_le_bytes(bytes(8)?.try_into()?)),
        "ByteProperty"
            if tag
                .type_parameters
                .first()
                .is_none_or(|enum_name| enum_name == "None") =>
        {
            json!(bytes(1)?[0])
        }
        other => bail!("{other}: only numbers and true/false can be changed so far"),
    })
}

/// Change a number or true/false in place. With `expect`, stop unless the current value is that
/// (so an edit made for one game version can't silently change something else after an update).
/// Returns the old value.
fn set_property(data: &mut [u8], edit: &SaveEdit) -> Result<Value> {
    let tag = locate(data, &edit.property, edit.occurrence, edit.after.as_deref())?;
    let old = fixed_value(data, &tag)
        .with_context(|| format!("{} is a {}", edit.property, tag.type_name))?;
    if let Some(expected) = &edit.expect {
        let same = match (old.as_f64(), expected.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => &old == expected,
        };
        if !same {
            bail!(
                "{} is {old}, not the expected {expected} (has the game's save changed?)",
                edit.property
            );
        }
    }
    let value = &edit.value;
    let at = tag.value_position;
    let whole = |value: &Value| -> Result<i64> {
        value
            .as_i64()
            .with_context(|| format!("{} is a {}, not {value}", edit.property, tag.type_name))
    };
    let number = |value: &Value| -> Result<f64> {
        value
            .as_f64()
            .with_context(|| format!("{} is a {}, not {value}", edit.property, tag.type_name))
    };
    let mut write = |bytes: &[u8]| data[at..at + bytes.len()].copy_from_slice(bytes);
    match tag.type_name.as_str() {
        "BoolProperty" => {
            let on = value
                .as_bool()
                .with_context(|| format!("{} is true/false, not {value}", edit.property))?;
            let flags = &mut data[tag.flags_position];
            *flags = if on { *flags | 0x10 } else { *flags & !0x10 };
        }
        "IntProperty" => write(&(whole(value)? as i32).to_le_bytes()),
        "UInt32Property" => write(&(whole(value)? as u32).to_le_bytes()),
        "Int64Property" => write(&whole(value)?.to_le_bytes()),
        "UInt64Property" => write(&(whole(value)? as u64).to_le_bytes()),
        "Int8Property" => write(&(whole(value)? as i8).to_le_bytes()),
        "Int16Property" => write(&(whole(value)? as i16).to_le_bytes()),
        "UInt16Property" => write(&(whole(value)? as u16).to_le_bytes()),
        "ByteProperty" => write(&[whole(value)? as u8]),
        "FloatProperty" => write(&(number(value)? as f32).to_le_bytes()),
        "DoubleProperty" => write(&number(value)?.to_le_bytes()),
        other => bail!("{other}: only numbers and true/false can be changed so far"),
    }
    Ok(old)
}

/// One edit to a save (a mod's `save/DefaultGame.json`).
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct SaveEdit {
    pub property: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub occurrence: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// `"header"` for the save's header; the body otherwise.
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Apply edits to a save's bytes. Returns the new bytes and each edit's old value.
pub fn edit_save(data: &[u8], edits: &[SaveEdit]) -> Result<(Vec<u8>, Vec<Value>)> {
    let (_, offset) = read_header(data)?;
    let mut header = data[..offset].to_vec();
    let mut body = decompress_body(data, offset)?;
    let mut old = vec![];
    for edit in edits {
        let target = if edit.part.as_deref() == Some("header") {
            &mut header
        } else {
            &mut body
        };
        old.push(set_property(target, edit)?);
    }
    let mut source = header;
    source.extend_from_slice(&data[offset..]);
    let out = write_body(&source, offset, &body)?;
    if decompress_body(&out, offset)? != body {
        bail!("the rewritten body doesn't decompress to the edited body");
    }
    Ok((out, old))
}

/// Write a template save into `<base>.pak`, with an empty IoStore container (`<base>.utoc/.ucas`)
/// beside it: a plain `.pak` in `~mods` isn't read (found 2026-10-18), but one with a container is.
pub fn write_template_pak(
    folder: &std::path::Path,
    base: &str,
    save: &[u8],
    game: &crate::game::Game,
) -> Result<()> {
    let utoc = folder.join(format!("{base}.utoc"));
    crate::package::write_container(
        &utoc,
        &[],
        retoc::version::EngineVersion::UE5_6,
        crate::package::script_objects(game)?,
    )?;
    let file = std::fs::File::create(utoc.with_extension("pak"))?;
    let mut writer = repak::PakBuilder::new().writer(
        std::io::BufWriter::new(file),
        repak::Version::V11,
        "../../../".to_string(),
        None,
    );
    writer.write_file(TEMPLATE, false, save)?;
    writer.write_index()?;
    Ok(())
}
