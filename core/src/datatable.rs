//! Decode cooked (unversioned-property) objects, such as DataTables and curves, from legacy
//! `.uasset`/`.uexp` pairs. All layouts come from the schemas (a `.usmap`).

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::reader::{Package, Reader};
use crate::schema::{Property, Schemas};
use crate::text::{Localizer, Text};

/// Structs the engine serializes natively, as their fields in order with no property header, rather
/// than as unversioned properties. Their fields come from the schemas like any other struct.
pub const NATIVE_STRUCTS: &[&str] = &[
    "Color",
    "DeprecateSlateVector2D",
    "Guid",
    "IntPoint",
    "IntVector",
    "LinearColor",
    "Quat",
    "RichCurveKey",
    "Rotator",
    "Vector",
    "Vector2D",
    "Vector2f",
    "Vector4",
];

/// JSON object keys must be strings: string keys as-is, anything else as its JSON text.
fn key_string(key: &Value) -> String {
    match key {
        Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

fn float(number: f64) -> Value {
    json!(number)
}

/// Store `value` as `property`'s value, or as element `element` of it for a fixed-size array.
fn store(out: &mut Map<String, Value>, property: &Property, element: usize, value: Value) {
    match out.get_mut(&property.name) {
        Some(Value::Array(elements)) if property.array_dimension > 1 => elements[element] = value,
        _ => drop(out.insert(property.name.clone(), value)),
    }
}

/// Where each decoded value was in the data: its path (property names and element indices joined
/// by `/`, e.g. `Processor/GridConfigList/0/position`) -> (start, end). A property the header marks
/// as zero has an empty span; one the file doesn't store has none. The table writer uses these to
/// copy unchanged values byte for byte.
pub type Spans = HashMap<String, (usize, usize)>;

pub struct Decoder<'a> {
    pub reader: Reader<'a>,
    /// Resolves object references to paths.
    package: &'a Package,
    schemas: &'a Schemas,
    /// Resolve texts to strings; without one, texts are kept as objects with their keys.
    localizer: Option<&'a Localizer>,
    /// Recorded when set (see [`Spans`]); `path` is where the value being decoded goes.
    pub spans: Option<Spans>,
    pub path: Vec<String>,
}

impl<'a> Decoder<'a> {
    pub fn new(
        data: &'a [u8],
        package: &'a Package,
        schemas: &'a Schemas,
        localizer: Option<&'a Localizer>,
    ) -> Self {
        Decoder {
            reader: Reader::new(data, &package.names),
            package,
            schemas,
            localizer,
            spans: None,
            path: vec![],
        }
    }

    /// Decode a value as `key` below the current path, recording its span.
    fn child<T>(&mut self, key: String, value: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.path.push(key);
        let start = self.reader.position;
        let result = value(self);
        if let (Some(spans), Ok(_)) = (&mut self.spans, &result) {
            spans.insert(self.path.join("/"), (start, self.reader.position));
        }
        self.path.pop();
        result
    }

    pub fn text(&mut self) -> Result<Value> {
        let reader = &mut self.reader;
        let flags = reader.u32()?;
        let text = match reader.i8()? {
            -1 => Text::Plain(if reader.u32()? != 0 {
                reader.fstring()?
            } else {
                String::new()
            }),
            0 => Text::Localized {
                namespace: reader.fstring()?,
                key: reader.fstring()?,
                source: reader.fstring()?,
            },
            11 => Text::Table {
                table: reader.fname()?,
                key: reader.fstring()?,
            },
            history => bail!(
                "unsupported FText history {history} at {:#x}",
                reader.position
            ),
        };
        let mut value = match (self.localizer, text) {
            (Some(localizer), text) => return Ok(Value::String(localizer.text(&text))),
            (None, Text::Plain(string)) if flags == 0 => return Ok(Value::String(string)),
            (None, Text::Plain(string)) => json!({"string": string}),
            (
                None,
                Text::Localized {
                    namespace,
                    key,
                    source,
                },
            ) => {
                json!({"namespace": namespace, "key": key, "source": source})
            }
            (None, Text::Table { table, key }) => json!({"table": table, "key": key}),
        };
        // Kept as objects, texts keep their flags too, so they can be written back unchanged
        if flags != 0 {
            value["flags"] = json!(flags);
        }
        Ok(value)
    }

    /// An object's own data: its class's properties, then the `bHasGuid` flag (and GUID).
    pub fn object(&mut self, class: &str) -> Result<Value> {
        let properties = self.struct_value(class)?;
        if self.reader.u32()? != 0 {
            self.reader.take(16)?; // GUID
        }
        Ok(properties)
    }

    pub fn struct_value(&mut self, name: &str) -> Result<Value> {
        if name == "GameplayTagContainer" {
            // Serialized natively as just its tag names.
            let count = self.reader.i32()?;
            return (0..count)
                .map(|_| self.reader.fname().map(Value::String))
                .collect();
        }
        let properties = self.schemas.all_properties(name)?;
        if NATIVE_STRUCTS.contains(&name) {
            let mut out = self.zero_struct(name)?; // every field is stored
            for (property, element) in properties {
                let value = self.value(property)?;
                store(&mut out, property, element, value);
            }
            return Ok(Value::Object(out));
        }
        // A property the file doesn't store has the struct's default value, which comes from the
        // game's code and isn't known here, so it's null. One flagged as zero really is zero.
        let mut out = self.struct_of(name, |_| Ok(Value::Null))?;
        for (index, is_zero) in self.reader.unversioned_header()? {
            let Some(&(property, element)) = properties.get(index) else {
                bail!("{name}: property index {index} out of range")
            };
            let value = self.child(property_key(property, element), |decoder| {
                if is_zero {
                    zero_value(decoder.schemas, property)
                } else {
                    decoder
                        .value(property)
                        .with_context(|| format!("{name}.{}", property.name))
                }
            })?;
            store(&mut out, property, element, value);
        }
        Ok(Value::Object(out))
    }

    /// Every property of a struct at its zero value (fixed-size arrays as arrays of zeros).
    fn zero_struct(&self, name: &str) -> Result<Map<String, Value>> {
        zero_struct(self.schemas, name)
    }

    /// Every property of a struct at the value `fill` gives it (fixed-size arrays as arrays of it).
    fn struct_of(
        &self,
        name: &str,
        fill: impl Fn(&Property) -> Result<Value>,
    ) -> Result<Map<String, Value>> {
        struct_of(self.schemas, name, fill)
    }

    fn enum_name(&self, property: &Property, value: i64) -> Value {
        enum_name(self.schemas, property, value)
    }

    fn value(&mut self, property: &Property) -> Result<Value> {
        let reader = &mut self.reader;
        Ok(match property.type_name.as_str() {
            "Int8" => json!(reader.i8()?),
            "Int16" => json!(reader.i16()?),
            "Int" => json!(reader.i32()?),
            "Int64" => json!(reader.i64()?),
            "UInt16" => json!(reader.u16()?),
            "UInt32" => json!(reader.u32()?),
            "UInt64" => json!(reader.u64()?),
            "Float" => float(reader.f32()? as f64),
            "Double" | "LargeWorldCoordinatesReal" => float(reader.f64()?),
            "Byte" => {
                let value = reader.u8()? as i64;
                if property.enum_name.is_some() {
                    self.enum_name(property, value)
                } else {
                    json!(value)
                }
            }
            "Enum" => {
                let value = match property.underlying.as_deref().unwrap_or("Byte") {
                    "Byte" => reader.u8()? as i64,
                    "Int" => reader.i32()? as i64,
                    "UInt16" => reader.u16()? as i64,
                    "UInt32" => reader.u32()? as i64,
                    "Int64" => reader.i64()?,
                    underlying => bail!("unsupported enum underlying type {underlying}"),
                };
                self.enum_name(property, value)
            }
            "Bool" => json!(reader.u8()? != 0),
            "Name" => json!(reader.fname()?),
            "Str" => json!(reader.fstring()?),
            "Text" => self.text()?,
            "Object" | "Class" | "Interface" | "WeakObject" | "LazyObject" => match reader.i32()? {
                0 => Value::Null,
                index => match self.package.object_path(index) {
                    Some(path) => json!(path),
                    None => bail!("object reference {index} isn't in the package"),
                },
            },
            "SoftObject" | "SoftClass" => {
                let (package_name, asset_name, sub_path) =
                    (reader.fname()?, reader.fname()?, reader.fstring()?);
                match package_name.as_str() {
                    "None" => Value::Null,
                    _ if sub_path.is_empty() => json!(format!("{package_name}.{asset_name}")),
                    _ => json!(format!("{package_name}.{asset_name}:{sub_path}")),
                }
            }
            "Struct" => self.struct_value(property.struct_name.as_deref().unwrap_or_default())?,
            "Array" | "Set" => {
                if property.type_name == "Set" {
                    let removed = reader.i32()?;
                    if removed != 0 {
                        bail!("a set listing {removed} elements to remove isn't supported");
                    }
                }
                let inner = property
                    .inner
                    .as_deref()
                    .context("container without inner property")?;
                let count = self.reader.i32()?;
                (0..count)
                    .map(|i| self.child(i.to_string(), |decoder| decoder.element(inner)))
                    .collect::<Result<Vec<_>>>()?
                    .into()
            }
            "Map" => {
                let removed = reader.i32()?;
                if removed != 0 {
                    bail!("a map listing {removed} keys to remove isn't supported");
                }
                let key_property = property.key.as_deref().context("map without key")?;
                let value_property = property.value.as_deref().context("map without value")?;
                let mut out = Map::new();
                for _ in 0..self.reader.i32()? {
                    let key = key_string(&self.element(key_property)?);
                    out.insert(key, self.element(value_property)?);
                }
                out.into()
            }
            type_name => bail!(
                "unsupported property type {type_name} at {:#x}",
                reader.position
            ),
        })
    }

    /// Container elements: enums are stored as FNames ("EType::Value") rather than numbers.
    fn element(&mut self, property: &Property) -> Result<Value> {
        if property.type_name == "Enum" {
            return Ok(json!(self.reader.fname()?.rsplit("::").next().unwrap()));
        }
        self.value(property)
    }

    /// Fail unless exactly the 4-byte package tag is left, which shows the layout was right.
    fn finish(&self) -> Result<()> {
        let left = self.reader.data.len() - self.reader.position;
        if left != 4 {
            bail!("{left} bytes left after decoding (expected 4); the layout is probably wrong");
        }
        Ok(())
    }
}

/// A property's key in a [`Spans`] path: its name, plus the element for a fixed-size array.
pub fn property_key(property: &Property, element: usize) -> String {
    if property.array_dimension > 1 {
        format!("{}/{element}", property.name)
    } else {
        property.name.clone()
    }
}

/// Every property of a struct at its zero value (fixed-size arrays as arrays of zeros).
pub fn zero_struct(schemas: &Schemas, name: &str) -> Result<Map<String, Value>> {
    struct_of(schemas, name, |property| zero_value(schemas, property))
}

/// Every property of a struct at the value `fill` gives it (fixed-size arrays as arrays of it).
fn struct_of(
    schemas: &Schemas,
    name: &str,
    fill: impl Fn(&Property) -> Result<Value>,
) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for (property, _) in schemas.all_properties(name)? {
        if !out.contains_key(&property.name) {
            let value = fill(property)?;
            let value = match property.array_dimension {
                0 | 1 => value,
                count => Value::Array(vec![value; count as usize]),
            };
            out.insert(property.name.clone(), value);
        }
    }
    Ok(out)
}

/// The value a property has when the header marks it as zero.
pub fn zero_value(schemas: &Schemas, property: &Property) -> Result<Value> {
    Ok(match property.type_name.as_str() {
        "Struct" => match property.struct_name.as_deref().unwrap_or_default() {
            "GameplayTagContainer" => json!([]),
            struct_name => zero_struct(schemas, struct_name)?.into(),
        },
        "Enum" => enum_name(schemas, property, 0),
        "Byte" if property.enum_name.is_some() => enum_name(schemas, property, 0),
        "Byte" | "Int8" | "Int16" | "Int" | "Int64" | "UInt16" | "UInt32" | "UInt64" => {
            json!(0)
        }
        "Float" | "Double" | "LargeWorldCoordinatesReal" => float(0.0),
        "Bool" => json!(false),
        "Name" => json!("None"),
        "Str" | "Text" => json!(""),
        "Array" | "Set" => json!([]),
        "Map" => json!({}),
        _ => Value::Null,
    })
}

/// An enum value's name, or the number if it has none (e.g. craft type 26, past `Max`).
pub fn enum_name(schemas: &Schemas, property: &Property, value: i64) -> Value {
    let name = property
        .enum_name
        .as_ref()
        .and_then(|enum_name| schemas.enums.get(enum_name))
        .and_then(|values| values.get(&value.to_string()));
    name.map_or_else(|| json!(value), |name| json!(name))
}

/// The class of a package's main object, e.g. `DataTable`.
pub fn main_class(package: &Package) -> Result<&str> {
    package
        .export_classes
        .first()
        .map(String::as_str)
        .context("package has no exports")
}

/// Decode a DataTable to `{"row_struct": ..., "rows": {name: row}}` (plus `"parent_tables"` for a
/// CompositeDataTable). Its class and row struct must be in `schemas`.
pub fn read_datatable(
    (uasset, uexp): (&[u8], &[u8]),
    schemas: &Schemas,
    localizer: Option<&Localizer>,
) -> Result<Value> {
    let package = Package::read(uasset)?;
    let mut decoder = Decoder::new(uexp, &package, schemas, localizer);
    let class = main_class(&package)?;
    let properties = decoder.object(class)?;
    let row_struct_path = properties["RowStruct"]
        .as_str()
        .context("DataTable has no RowStruct")?;
    let row_struct = row_struct_path.rsplit('.').next().unwrap().to_string();
    let mut rows = Map::new();
    for _ in 0..decoder.reader.i32()? {
        let name = decoder.reader.fname()?;
        let row = decoder
            .struct_value(&row_struct)
            .with_context(|| format!("row {name}"))?;
        rows.insert(name, row);
    }
    decoder.finish()?;
    let mut out = json!({"row_struct": row_struct});
    if class == "CompositeDataTable" {
        out["parent_tables"] = properties["ParentTables"].clone();
    }
    out["rows"] = rows.into();
    Ok(out)
}

/// Decode an object whose data is only its properties, such as a curve, to those properties.
/// Its class must be in `schemas`.
pub fn read_object(
    (uasset, uexp): (&[u8], &[u8]),
    schemas: &Schemas,
    localizer: Option<&Localizer>,
) -> Result<Value> {
    let package = Package::read(uasset)?;
    let mut decoder = Decoder::new(uexp, &package, schemas, localizer);
    let properties = decoder.object(main_class(&package)?)?;
    decoder.finish()?;
    Ok(properties)
}
