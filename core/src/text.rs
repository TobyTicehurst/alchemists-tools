//! Localisation: StringTable assets and .locres files.
//!
//! StringTables hold the *source* (Chinese) strings; translations live in
//! `Content/Localization/Game/<language>/Game.locres`, keyed by (namespace, key).

use std::collections::HashMap;

use anyhow::{Context, Result};
use indexmap::IndexMap;

use crate::reader::{Package, Reader};

/// A decoded FText.
#[derive(Clone, Debug)]
pub enum Text {
    /// Culture-invariant / untranslated string.
    Plain(String),
    /// Localisable text: translated by (namespace, key); `source` is the original string.
    Localized {
        namespace: String,
        key: String,
        source: String,
    },
    /// Entry of a StringTable asset (`/Game/DataTables/ST_X.ST_X`).
    Table { table: String, key: String },
}

/// namespace -> key -> string
pub type Locres = HashMap<String, HashMap<String, String>>;

pub fn read_locres(data: &[u8]) -> Result<Locres> {
    let mut reader = Reader::new(data, &[]);
    reader.position = 16; // magic GUID
    let version = reader.u8()?;
    let strings_offset = reader.i64()? as usize;
    if version >= 2 {
        reader.u32()?; // entry count
    }
    let mut namespaces = vec![];
    for _ in 0..reader.u32()? {
        if version >= 2 {
            reader.u32()?; // hash
        }
        let namespace = reader.fstring()?;
        let mut keys = vec![];
        for _ in 0..reader.u32()? {
            if version >= 2 {
                reader.u32()?;
            }
            let key = reader.fstring()?;
            reader.u32()?; // source string hash
            keys.push((key, reader.i32()? as usize));
        }
        namespaces.push((namespace, keys));
    }
    reader.position = strings_offset;
    let mut strings = vec![];
    for _ in 0..reader.i32()? {
        strings.push(reader.fstring()?);
        if version >= 3 {
            reader.i32()?; // reference count
        }
    }
    Ok(namespaces
        .into_iter()
        .map(|(namespace, keys)| {
            let entries = keys
                .into_iter()
                .filter_map(|(key, i)| Some((key, strings.get(i)?.clone())))
                .collect();
            (namespace, entries)
        })
        .collect())
}

/// A StringTable asset: (namespace, key -> source string).
pub fn read_string_table(uasset: &[u8], uexp: &[u8]) -> Result<(String, IndexMap<String, String>)> {
    let names = Package::read(uasset)?.names;
    let mut reader = Reader::new(uexp, &names);
    reader.unversioned_header()?; // UStringTable sets no properties
    reader.u32()?; // bHasGuid
    let namespace = reader.fstring()?;
    let mut entries = IndexMap::new();
    for _ in 0..reader.i32()? {
        let key = reader.fstring()?;
        entries.insert(key, reader.fstring()?);
    }
    Ok((namespace, entries))
}

/// Resolves [`Text`] to one language.
pub struct Localizer {
    /// object path -> (namespace, entries)
    tables: HashMap<String, (String, IndexMap<String, String>)>,
    translations: Locres,
}

impl Localizer {
    /// The string tables are given as (asset path without extension, `.uasset`, `.uexp`).
    pub fn new<'a>(
        string_tables: impl IntoIterator<Item = (&'a str, &'a [u8], &'a [u8])>,
        locres: &[u8],
    ) -> Result<Self> {
        let mut tables = HashMap::new();
        for (path, uasset, uexp) in string_tables {
            let table =
                read_string_table(uasset, uexp).with_context(|| format!("reading {path}"))?;
            tables.insert(object_path(path), table);
        }
        Ok(Localizer {
            tables,
            translations: read_locres(locres)?,
        })
    }

    pub fn lookup(&self, namespace: &str, key: &str) -> Option<&String> {
        self.translations.get(namespace)?.get(key)
    }

    pub fn text(&self, text: &Text) -> String {
        match text {
            Text::Plain(string) => string.clone(),
            Text::Localized {
                namespace,
                key,
                source,
            } => match key.is_empty() {
                true => source.clone(),
                false => self.lookup(namespace, key).unwrap_or(source).clone(),
            },
            Text::Table { table, key } => {
                let Some((namespace, entries)) = self.tables.get(table) else {
                    return key.clone();
                };
                self.lookup(namespace, key)
                    .or_else(|| entries.get(key))
                    .unwrap_or(key)
                    .clone()
            }
        }
    }
}

/// `AlchemyFactory/Content/DataTables/ST_X` -> `/Game/DataTables/ST_X.ST_X`
fn object_path(path: &str) -> String {
    let relative = path.split_once("/Content/").map_or(path, |(_, rest)| rest);
    let leaf = relative.rsplit('/').next().unwrap_or(relative);
    format!("/Game/{relative}.{leaf}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn object_path() {
        let path = "AlchemyFactory/Content/DataTables/ST_Localization_Names";
        assert_eq!(
            super::object_path(path),
            "/Game/DataTables/ST_Localization_Names.ST_Localization_Names"
        );
    }
}
