//! `.usmap` mappings (as dumped by UE4SS / UnrealMappingsDumper) -> [`Schemas`].
//!
//! Layout: u16 magic 0x30C4, u8 version, [i32 has-versioning (+ versions)], u8 compression,
//! u32 compressed size, u32 size, then: names (u16 length + UTF-8), enums, structs. Each struct
//! lists its serialized properties by schema index, which is the unversioned property index.

use std::path::Path;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;

use crate::game::display_path;
use crate::reader::Reader;
use crate::schema::{Property, Schemas, Struct};

const MAGIC: u16 = 0x30C4;
const VERSION_PACKAGE_VERSIONING: u8 = 1;
const VERSION_LONG_FNAME: u8 = 2;
const VERSION_LARGE_ENUMS: u8 = 3;
const VERSION_EXPLICIT_ENUM_VALUES: u8 = 4;

/// usmap property type ids -> the type names used in [`Property::type_name`].
const TYPES: [&str; 31] = [
    "Byte",
    "Bool",
    "Int",
    "Float",
    "Object",
    "Name",
    "Delegate",
    "Double",
    "Array",
    "Struct",
    "Str",
    "Text",
    "Interface",
    "MulticastDelegate",
    "WeakObject",
    "LazyObject",
    "SoftObject",
    "SoftObject",
    "UInt64",
    "UInt32",
    "UInt16",
    "Int64",
    "Int16",
    "Int8",
    "Map",
    "Set",
    "Enum",
    "FieldPath",
    "Optional",
    "Utf8Str",
    "AnsiStr",
];

pub fn read_usmap(path: &Path) -> Result<Schemas> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", display_path(path)))?;
    let mut reader = Reader::new(&data, &[]);
    if reader.u16()? != MAGIC {
        bail!("{} is not a .usmap file", display_path(path));
    }
    let version = reader.u8()?;
    if version > VERSION_EXPLICIT_ENUM_VALUES {
        bail!("unsupported .usmap version {version}");
    }
    if version >= VERSION_PACKAGE_VERSIONING && reader.i32()? != 0 {
        reader.take(8)?; // package file version (UE4, UE5)
        let custom_versions = reader.i32()?;
        reader.take(custom_versions as usize * 20)?; // each: GUID + version
        reader.u32()?; // net changelist
    }
    let compression = reader.u8()?;
    let (_compressed_size, size) = (reader.u32()?, reader.u32()?);
    if compression != 0 {
        bail!(
            "compressed .usmap files aren't supported yet (method {compression}); \
             dump without compression"
        );
    }
    let body = reader.take(size as usize)?;
    Body {
        reader: Reader::new(body, &[]),
        version,
        names: vec![],
    }
    .read()
}

struct Body<'a> {
    reader: Reader<'a>,
    version: u8,
    names: Vec<String>,
}

impl Body<'_> {
    fn name(&mut self) -> Result<String> {
        let index = self.reader.i32()?;
        match usize::try_from(index).ok().and_then(|i| self.names.get(i)) {
            Some(name) => Ok(name.clone()),
            None => bail!("name index {index} out of range"),
        }
    }

    /// A name index where -1 means none.
    fn optional_name(&mut self) -> Result<Option<String>> {
        let position = self.reader.position;
        if self.reader.i32()? == -1 {
            return Ok(None);
        }
        self.reader.position = position;
        self.name().map(Some)
    }

    fn read(mut self) -> Result<Schemas> {
        for _ in 0..self.reader.u32()? {
            let length = if self.version >= VERSION_LONG_FNAME {
                self.reader.u16()? as usize
            } else {
                self.reader.u8()? as usize
            };
            self.names
                .push(String::from_utf8_lossy(self.reader.take(length)?).into_owned());
        }

        let mut schemas = Schemas::default();
        for _ in 0..self.reader.u32()? {
            let name = self.name()?;
            let count = if self.version >= VERSION_LARGE_ENUMS {
                self.reader.u16()? as i64
            } else {
                self.reader.u8()? as i64
            };
            let mut values = IndexMap::new();
            for i in 0..count {
                let value = if self.version >= VERSION_EXPLICIT_ENUM_VALUES {
                    self.reader.i64()?
                } else {
                    i
                };
                let enumerator = self.name()?;
                values.insert(
                    value.to_string(),
                    enumerator.rsplit("::").next().unwrap().to_string(),
                );
            }
            schemas.enums.insert(name, values);
        }

        for _ in 0..self.reader.u32()? {
            let name = self.name()?;
            let super_name = self.optional_name()?;
            let (_count, serializable) = (self.reader.u16()?, self.reader.u16()?);
            let mut indexed = vec![];
            for _ in 0..serializable {
                let index = self.reader.u16()?;
                let array_dimension = self.reader.u8()? as u16;
                let property_name = self.name()?;
                let mut property = self.property_type()?;
                property.name = property_name;
                property.array_dimension = array_dimension;
                indexed.push((index, property));
            }
            let properties = in_index_order(indexed);
            schemas.structs.insert(
                name.clone(),
                Struct {
                    name,
                    super_name,
                    properties,
                },
            );
        }
        Ok(schemas)
    }

    fn property_type(&mut self) -> Result<Property> {
        let type_id = self.reader.u8()?;
        let Some(type_name) = TYPES.get(type_id as usize) else {
            bail!("unknown .usmap property type {type_id}")
        };
        let mut property = Property {
            type_name: type_name.to_string(),
            array_dimension: 1,
            ..Default::default()
        };
        match *type_name {
            "Enum" => {
                property.underlying = Some(self.property_type()?.type_name);
                property.enum_name = Some(self.name()?);
            }
            "Struct" => property.struct_name = Some(self.name()?),
            "Array" | "Set" | "Optional" => property.inner = Some(Box::new(self.property_type()?)),
            "Map" => {
                property.key = Some(Box::new(self.property_type()?));
                property.value = Some(Box::new(self.property_type()?));
            }
            _ => {}
        }
        Ok(property)
    }
}

/// Order properties by schema index. Each occupies `array_dimension` indices; any gap (a property
/// that is never serialized) is filled so later indices still line up.
fn in_index_order(mut indexed: Vec<(u16, Property)>) -> Vec<Property> {
    indexed.sort_by_key(|(index, _)| *index);
    let mut properties = vec![];
    let mut next = 0;
    for (index, property) in indexed {
        while next < index {
            properties.push(Property {
                name: format!("_unserialized{next}"),
                type_name: "Unknown".into(),
                array_dimension: 1,
                ..Default::default()
            });
            next += 1;
        }
        next = index + property.array_dimension.max(1);
        properties.push(property);
    }
    properties
}
