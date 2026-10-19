//! Reading cooked Blueprints and other cooked assets: their top-level objects and their component
//! templates, decoded with the class layouts in the `.usmap`.

use anyhow::Result;
use indexmap::IndexMap;
use serde_json::Value;

use crate::datatable::Decoder;
use crate::reader::Package;
use crate::schema::Schemas;

/// A Blueprint's component template (an export named `<name>_GEN_VARIABLE`).
pub struct Component {
    pub name: String,
    /// Its class, e.g. `StaticMeshComponent`.
    pub class: String,
    /// The component it's attached to, from the construction script's nodes.
    pub parent: Option<String>,
    /// Its stored properties (null if its class layout isn't in the `.usmap`).
    pub values: Value,
}

const COMPONENT_SUFFIX: &str = "_GEN_VARIABLE";

/// A cooked package's top-level objects: object path -> class path, e.g.
/// `/Game/Mods/X/T_Icon.T_Icon` -> `/Script/Engine.Texture2D`.
pub fn assets(uasset: &[u8]) -> Result<IndexMap<String, String>> {
    let package = Package::read(uasset)?;
    Ok(package
        .export_paths
        .iter()
        .zip(&package.export_class_paths)
        .zip(&package.export_outers)
        .filter(|(_, outer)| **outer == 0)
        .map(|((path, class), _)| (path.clone(), class.clone()))
        .collect())
}

/// An export's stored properties, or null when they can't be decoded.
fn decode(
    package: &Package,
    uasset_length: usize,
    uexp: &[u8],
    schemas: &Schemas,
    i: usize,
) -> Value {
    let (offset, size) = package.export_data[i];
    let start = (offset as usize).saturating_sub(uasset_length);
    let Some(data) = uexp.get(start..start + size as usize) else {
        return Value::Null;
    };
    let mut decoder = Decoder::new(data, package, schemas, None);
    decoder
        .struct_value(&package.export_classes[i])
        .unwrap_or(Value::Null)
}

/// A Blueprint's component templates, in export order.
pub fn components(uasset: &[u8], uexp: &[u8], schemas: &Schemas) -> Result<Vec<Component>> {
    let package = Package::read(uasset)?;
    let nodes: Vec<Value> = (0..package.exports.len())
        .filter(|&i| package.export_classes[i] == "SCS_Node")
        .map(|i| decode(&package, uasset.len(), uexp, schemas, i))
        .collect();
    // A node names its component (InternalVariableName) and lists its children (as node objects)
    let variable = |node_path: &str| {
        let name = node_path.rsplit(['.', ':']).next().unwrap_or(node_path);
        (0..package.exports.len())
            .filter(|&i| package.export_classes[i] == "SCS_Node" && package.exports[i] == name)
            .map(|i| decode(&package, uasset.len(), uexp, schemas, i))
            .find_map(|node| {
                node.get("InternalVariableName")?
                    .as_str()
                    .map(str::to_string)
            })
    };
    let mut parents: IndexMap<String, String> = IndexMap::new();
    for node in &nodes {
        let Some(parent) = node.get("InternalVariableName").and_then(Value::as_str) else {
            continue;
        };
        for child in node
            .get("ChildNodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(child) = child.as_str().and_then(variable) {
                parents.insert(child, parent.to_string());
            }
        }
    }
    Ok((0..package.exports.len())
        .filter_map(|i| {
            let name = package.exports[i]
                .strip_suffix(COMPONENT_SUFFIX)?
                .to_string();
            Some(Component {
                parent: parents.get(&name).cloned(),
                class: package.export_classes[i].clone(),
                values: decode(&package, uasset.len(), uexp, schemas, i),
                name,
            })
        })
        .collect())
}
