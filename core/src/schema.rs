//! Struct and enum layouts ("schemas"): which properties each struct has, in serialization order.
//!
//! Cooked assets use *unversioned property serialization*, which stores no property names or
//! types, so a decoder needs these. They come from a `.usmap` mappings file (see [`crate::usmap`]).

use std::path::Path;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::game::display_path;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Property {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub array_dimension: u16,
    #[serde(rename = "struct", skip_serializing_if = "Option::is_none", default)]
    pub struct_name: Option<String>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none", default)]
    pub enum_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub inner: Option<Box<Property>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<Box<Property>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<Box<Property>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub underlying: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Struct {
    pub name: String,
    #[serde(rename = "super")]
    pub super_name: Option<String>,
    pub properties: Vec<Property>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Schemas {
    pub structs: IndexMap<String, Struct>,
    /// enum name -> value (as a string) -> enumerator name
    pub enums: IndexMap<String, IndexMap<String, String>>,
}

impl Schemas {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", display_path(path)))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn get(&self, name: &str) -> Result<&Struct> {
        match self.structs.get(name) {
            Some(found) => Ok(found),
            None => bail!("no schema for struct {name}"),
        }
    }

    /// Only the given structs and everything they use (supers, nested structs, enums).
    pub fn subset(&self, roots: &[&str]) -> Schemas {
        fn visit(all: &Schemas, out: &mut Schemas, name: &str) {
            let Some(found) = all.structs.get(name) else {
                return;
            };
            if out.structs.contains_key(name) {
                return;
            }
            out.structs.insert(name.to_string(), found.clone());
            let mut properties: Vec<&Property> = found.properties.iter().collect();
            while let Some(property) = properties.pop() {
                let used_enum = property
                    .enum_name
                    .as_ref()
                    .and_then(|enum_name| all.enums.get_key_value(enum_name));
                if let Some((enum_name, values)) = used_enum {
                    out.enums.insert(enum_name.clone(), values.clone());
                }
                if let Some(struct_name) = &property.struct_name {
                    visit(all, out, struct_name);
                }
                let nested = [&property.inner, &property.key, &property.value];
                properties.extend(nested.into_iter().flatten().map(|boxed| &**boxed));
            }
            if let Some(super_name) = &found.super_name {
                visit(all, out, super_name);
            }
        }
        let mut out = Schemas::default();
        for root in roots {
            visit(self, &mut out, root);
        }
        out
    }

    /// Every property of a struct in unversioned-index order: its own, then its super's (as
    /// `TFieldIterator` walks them). A fixed-size array takes one index per element, so each
    /// entry is (property, element).
    pub fn all_properties(&self, name: &str) -> Result<Vec<(&Property, usize)>> {
        let found = self.get(name)?;
        let mut out = vec![];
        for property in &found.properties {
            let count = property.array_dimension.max(1) as usize;
            out.extend((0..count).map(|element| (property, element)));
        }
        if let Some(super_name) = &found.super_name {
            out.extend(self.all_properties(super_name)?);
        }
        Ok(out)
    }
}
