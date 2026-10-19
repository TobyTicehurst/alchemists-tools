//! Edit files: changes to the game's DataTables, written as JSON, turned into edited tables.
//!
//! An edit file is `{table: {row: fields}}`; every key starting with `_` is a comment, apart from
//! the row options `_copy` (a new row starting as a copy of another), `_slot` (a new recipe written
//! over one of the game's unused recipe rows) and `_unlocked`. Objects merge field by field, lists
//! are replaced whole, and every value is checked against its field's type. See MODDING.md in the
//! mod project for the user's view.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::datatable::{read_datatable, zero_struct, NATIVE_STRUCTS};
use crate::facts::machines_for;
use crate::package::LegacyAsset;
use crate::schema::{Property, Schemas};
use crate::text::Localizer;
use crate::unpack::Assets;
use crate::write::{split_reference, Table};

const RECIPES: &str = "DT_EnemyCrafting";
const NEW_RECIPE_REQUIRED: &[&str] =
    &["IngredientList", "ProductInfo", "CraftType", "CraftingTime"];
const OUTPUT_FIELDS: &[&str] = &["SideProduct", "FailProduct1", "FailProduct2"];
const INTEGER_TYPES: &[&str] = &[
    "Int8", "Int16", "Int", "Int64", "UInt16", "UInt32", "UInt64", "Byte",
];

/// One edit file: a label for messages (e.g. `MyMod/recipes.json`) and its contents.
pub struct Source {
    pub label: String,
    pub contents: Value,
}

/// What the edits are applied to.
pub struct Game<'a> {
    /// The game's tables, converted to legacy format (see [`crate::extract::content_assets`]).
    pub assets: &'a Assets,
    pub schemas: &'a Schemas,
    /// English names, for messages and suggestions; without it, messages use row names.
    pub localizer: Option<&'a Localizer>,
    /// The game's packages (`/Game/...`), to check references to game assets; without it,
    /// they aren't checked.
    pub packages: Option<&'a HashSet<String>>,
}

/// The edited tables and what was done.
pub struct Built {
    /// The changed tables, ready to pack.
    pub tables: Vec<LegacyAsset>,
    /// New rows: (table, row, the row it started from).
    pub new_rows: Vec<(String, String, String)>,
    /// table -> row (its name in the game) -> the complete row, for every changed row.
    pub changed: IndexMap<String, IndexMap<String, Value>>,
    /// References the tables didn't have before: object path -> class path.
    pub references: IndexMap<String, String>,
    /// Likely mistakes that don't stop the build.
    pub warnings: Vec<String>,
    /// What was changed, one line each, for the build's output.
    pub report: Vec<String>,
}

/// A row's parts: (file label, the fields it gives).
type Parts = Vec<(String, Value)>;

/// Tables and rows skipped because their names start with `_` but which hold rows or fields, so
/// were probably meant to apply (`mod new`'s example file turns its edits off this way). A
/// `_comment` holds text, so it isn't reported.
fn skipped(sources: &[Source]) -> Vec<String> {
    let has_entries = |value: &Value| {
        value
            .as_object()
            .is_some_and(|object| object.keys().any(|key| !key.starts_with('_')))
    };
    let mut out = vec![];
    for source in sources {
        let Some(tables) = source.contents.as_object() else {
            continue;
        };
        for (table, rows) in tables {
            if table.starts_with('_') {
                if has_entries(rows) {
                    out.push(format!(
                        "{}: {table} is skipped (its name starts with _); remove the _ to apply it",
                        source.label
                    ));
                }
                continue;
            }
            for (row, fields) in rows.as_object().into_iter().flatten() {
                if row.starts_with('_') && has_entries(fields) {
                    out.push(format!(
                        "{}: {table}.{row} is skipped (its name starts with _); remove the _ to \
                         apply it",
                        source.label
                    ));
                }
            }
        }
    }
    out
}

/// Every edit file merged: table -> row -> its parts in file order.
fn collect(sources: &[Source]) -> Result<IndexMap<String, IndexMap<String, Parts>>> {
    let mut out: IndexMap<String, IndexMap<String, Parts>> = IndexMap::new();
    for source in sources {
        let tables = source
            .contents
            .as_object()
            .with_context(|| format!("{}: an edit file is an object of tables", source.label))?;
        for (table, rows) in tables {
            if table.starts_with('_') {
                continue;
            }
            let rows = rows
                .as_object()
                .with_context(|| format!("{}: {table} must be an object of rows", source.label))?;
            for (row, fields) in rows {
                if row.starts_with('_') {
                    continue;
                }
                if !fields.is_object() {
                    bail!(
                        "{}: {table}.{row} must be an object of fields",
                        source.label
                    );
                }
                out.entry(table.clone())
                    .or_default()
                    .entry(row.clone())
                    .or_default()
                    .push((source.label.clone(), fields.clone()));
            }
        }
    }
    Ok(out)
}

/// The last value a row's parts give for an option (e.g. `_copy`).
fn option<'p>(parts: &'p Parts, key: &str) -> Option<&'p Value> {
    parts.iter().rev().find_map(|(_, fields)| fields.get(key))
}

fn is_true(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

/// A row's text, number or flag, treating a value the file doesn't store as unset.
fn get<'v>(row: &'v Value, field: &str) -> &'v Value {
    row.get(field).unwrap_or(&Value::Null)
}

fn flag(row: &Value, field: &str) -> bool {
    get(row, field).as_bool().unwrap_or(false)
}

fn name_of(value: &Value) -> &str {
    value.as_str().unwrap_or("None")
}

/// What the build needs to know about items.
struct Item {
    display: String,
    hidden: bool,
    fluid: bool,
}

struct Editor<'a> {
    game: &'a Game<'a>,
    items: IndexMap<String, Item>,
    /// Decoded tables (untranslated), each by its name.
    tables: HashMap<String, IndexMap<String, Value>>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl<'a> Editor<'a> {
    fn asset_path(&self, table: &str) -> Option<String> {
        let found = self.game.assets.find(table);
        match found.as_slice() {
            [path] => Some(path.to_string()),
            _ => None,
        }
    }

    /// A table's rows as the game has them (untranslated), decoded once.
    fn rows(&mut self, table: &str) -> Result<&IndexMap<String, Value>> {
        if !self.tables.contains_key(table) {
            let path = self
                .asset_path(table)
                .with_context(|| format!("no table {table} in the game"))?;
            let decoded = read_datatable(self.game.assets.asset(&path)?, self.game.schemas, None)?;
            let rows = decoded["rows"]
                .as_object()
                .context("table without rows")?
                .iter()
                .map(|(name, row)| (name.clone(), row.clone()))
                .collect();
            self.tables.insert(table.to_string(), rows);
        }
        Ok(&self.tables[table])
    }

    /// English names of a table's rows (DisplayName), if there's a localizer.
    fn display_names(&self, table: &str) -> HashMap<String, String> {
        let (Some(localizer), Some(path)) = (self.game.localizer, self.asset_path(table)) else {
            return HashMap::new();
        };
        let Ok(asset) = self.game.assets.asset(&path) else {
            return HashMap::new();
        };
        let Ok(decoded) = read_datatable(asset, self.game.schemas, Some(localizer)) else {
            return HashMap::new();
        };
        decoded["rows"]
            .as_object()
            .map(|rows| {
                rows.iter()
                    .filter_map(|(name, row)| {
                        let display = row.get("DisplayName")?.as_str()?;
                        Some((name.clone(), display.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- merging edits into rows, checked against the schema ---------------------------------

    /// The property `key` of a struct, matched without regard to case.
    fn property(&self, struct_name: &str, key: &str) -> Result<Option<&'a Property>> {
        let properties = self.game.schemas.all_properties(struct_name)?;
        let exact = properties.iter().find(|(property, _)| property.name == key);
        let found = exact.or_else(|| {
            properties
                .iter()
                .find(|(property, _)| property.name.eq_ignore_ascii_case(key))
        });
        Ok(found.map(|(property, _)| *property))
    }

    fn field_names(&self, struct_name: &str) -> Result<Vec<String>> {
        let mut names: Vec<String> = vec![];
        for (property, _) in self.game.schemas.all_properties(struct_name)? {
            if !names.contains(&property.name) {
                names.push(property.name.clone());
            }
        }
        Ok(names)
    }

    /// `edit` merged into `base` (a struct's value; null means stored as the default).
    fn merge_struct(
        &self,
        struct_name: &str,
        base: &Value,
        edit: &Value,
        at: &str,
    ) -> Result<Value> {
        let edit = edit
            .as_object()
            .with_context(|| format!("{at}: expected an object, got {edit}"))?;
        let mut out = match base {
            Value::Object(object) => object.clone(),
            _ => zero_struct(self.game.schemas, struct_name)?,
        };
        for (key, value) in edit {
            if key.starts_with('_') {
                continue;
            }
            let Some(property) = self.property(struct_name, key)? else {
                bail!(
                    "{at}: unknown field '{key}' (fields: {})",
                    self.field_names(struct_name)?.join(", ")
                );
            };
            let base_value = out.get(&property.name).cloned().unwrap_or(Value::Null);
            let merged = self.merge_property(
                property,
                &base_value,
                value,
                &format!("{at}.{}", property.name),
            )?;
            out.insert(property.name.clone(), merged);
        }
        Ok(Value::Object(out))
    }

    fn merge_property(
        &self,
        property: &Property,
        base: &Value,
        edit: &Value,
        at: &str,
    ) -> Result<Value> {
        if property.array_dimension > 1 {
            let elements = edit
                .as_array()
                .filter(|elements| elements.len() == property.array_dimension as usize)
                .with_context(|| {
                    format!(
                        "{at}: expected a list of {} values",
                        property.array_dimension
                    )
                })?;
            let single = Property {
                array_dimension: 1,
                ..property.clone()
            };
            return elements
                .iter()
                .enumerate()
                .map(|(i, element)| {
                    let base_element = base.get(i).unwrap_or(&Value::Null);
                    self.merge_property(&single, base_element, element, &format!("{at}[{i}]"))
                })
                .collect();
        }
        match property.type_name.as_str() {
            "Struct" => {
                let name = property.struct_name.as_deref().unwrap_or_default();
                if name == "GameplayTagContainer" {
                    let tags = edit
                        .as_array()
                        .filter(|tags| tags.iter().all(Value::is_string))
                        .with_context(|| format!("{at}: expected a list of gameplay tag names"))?;
                    return Ok(Value::Array(tags.clone()));
                }
                self.merge_struct(name, base, edit, at)
            }
            "Array" | "Set" => {
                // Lists are replaced whole: every entry, complete
                let entries = edit
                    .as_array()
                    .with_context(|| format!("{at}: expected a list, got {edit}"))?;
                let inner = property
                    .inner
                    .as_deref()
                    .context("list without inner property")?;
                let has_shape = base.as_array().is_some_and(|base| !base.is_empty());
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        self.list_entry(inner, has_shape, entry, &format!("{at}[{i}]"))
                    })
                    .collect()
            }
            "Map" => bail!("{at}: changing maps isn't supported"),
            _ => self.leaf(property, edit, at),
        }
    }

    fn list_entry(
        &self,
        inner: &Property,
        has_shape: bool,
        entry: &Value,
        at: &str,
    ) -> Result<Value> {
        let struct_name = inner.struct_name.as_deref().unwrap_or_default();
        if inner.type_name != "Struct" || struct_name == "GameplayTagContainer" {
            return self.merge_property(inner, &Value::Null, entry, at);
        }
        let given = entry
            .as_object()
            .with_context(|| format!("{at}: expected an object, got {entry}"))?;
        if has_shape || NATIVE_STRUCTS.contains(&struct_name) {
            let missing: Vec<String> = self
                .field_names(struct_name)?
                .into_iter()
                .filter(|name| !given.keys().any(|key| key.eq_ignore_ascii_case(name)))
                .collect();
            if !missing.is_empty() {
                bail!(
                    "{at}: list entries need every field (missing: {})",
                    missing.join(", ")
                );
            }
        }
        self.merge_struct(struct_name, &Value::Null, entry, at)
    }

    fn leaf(&self, property: &Property, value: &Value, at: &str) -> Result<Value> {
        let type_name = property.type_name.as_str();
        let is_enum = type_name == "Enum" || (type_name == "Byte" && property.enum_name.is_some());
        if is_enum {
            let names: Vec<&String> = property
                .enum_name
                .as_ref()
                .and_then(|name| self.game.schemas.enums.get(name))
                .map(|values| values.values().collect())
                .unwrap_or_default();
            if let Some(name) = value.as_str() {
                if names.iter().any(|known| *known == name) {
                    return Ok(value.clone());
                }
            }
            // a number is written as it is, even one without a name (e.g. craft type 26, past Max)
            if let Some(number) = value.as_u64().filter(|number| *number <= 255) {
                return Ok(json!(number));
            }
            let mut sorted: Vec<&String> = names;
            sorted.sort();
            let listed: Vec<&str> = sorted.iter().map(|name| name.as_str()).collect();
            bail!(
                "{at}: '{}' is not one of {} or a number 0-255",
                name_of(value),
                listed.join(", ")
            );
        }
        match type_name {
            _ if INTEGER_TYPES.contains(&type_name) => {
                if !value.is_i64() && !value.is_u64() {
                    bail!("{at}: expected a whole number, got {value}");
                }
            }
            "Float" | "Double" | "LargeWorldCoordinatesReal" => {
                if !value.is_number() {
                    bail!("{at}: expected a number, got {value}");
                }
            }
            "Bool" => {
                if !value.is_boolean() {
                    bail!("{at}: expected true or false, got {value}");
                }
            }
            "Name" => {
                let name = value
                    .as_str()
                    .with_context(|| format!("{at}: expected a name string, got {value}"))?;
                if property.name == "IngredientName"
                    && name != "None"
                    && !self.items.contains_key(name)
                {
                    bail!("{at}: unknown item '{name}'{}", self.suggest(name));
                }
            }
            "Str" | "Text" => {
                if !value.is_string() {
                    bail!("{at}: expected text, got {value}");
                }
            }
            "Object" | "Class" | "SoftObject" | "SoftClass" => {
                let path = value.as_str().map(|reference| split_reference(reference).1);
                if !value.is_null() && !path.is_some_and(|path| path.starts_with('/')) {
                    bail!(
                        "{at}: expected an object path such as \
                         \"/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C\" or \
                         \"/Script/Engine.Texture2D'/Game/Mods/MyMachine/T_Icon.T_Icon'\", got \
                         {value}"
                    );
                }
            }
            other => bail!("{at}: changing {other} fields isn't supported"),
        }
        Ok(value.clone())
    }

    /// " (did you mean: ...?)" for an unknown item name, from IDs and English names.
    fn suggest(&self, name: &str) -> String {
        let query = name.to_lowercase().replace(' ', "");
        let query = query.trim_end_matches('s');
        let hints: Vec<String> = self
            .items
            .iter()
            .filter(|(id, item)| {
                item.display.to_lowercase().replace(' ', "").contains(query)
                    || id.to_lowercase().contains(query)
            })
            .take(8)
            .map(|(id, item)| format!("{id} ({})", item.display))
            .collect();
        if hints.is_empty() {
            String::new()
        } else {
            format!(" (did you mean: {}?)", hints.join(", "))
        }
    }

    fn merge_parts(
        &self,
        row_struct: &str,
        base: &Value,
        parts: &Parts,
        at: &str,
    ) -> Result<Value> {
        let mut row = base.clone();
        for (label, fields) in parts {
            row = self.merge_struct(row_struct, &row, fields, &format!("{label}: {at}"))?;
        }
        Ok(row)
    }

    // --- new rows ----------------------------------------------------------------------------

    /// The game's unused recipe rows: hidden, making an item that's hidden or doesn't exist.
    fn free_recipe_slots(&self, rows: &IndexMap<String, Value>) -> Vec<String> {
        rows.iter()
            .filter(|(_, row)| flag(row, "bHideInGame"))
            .filter(|(_, row)| {
                let product = name_of(get(get(row, "ProductInfo"), "IngredientName"));
                self.items.get(product).is_none_or(|item| item.hidden)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// A complete recipe row for a new recipe, from its parts on top of `template`.
    fn new_recipe(
        &self,
        name: &str,
        parts: &Parts,
        template: &Value,
        rows: &IndexMap<String, Value>,
        row_struct: &str,
    ) -> Result<Value> {
        let given: HashSet<String> = parts
            .iter()
            .flat_map(|(_, fields)| {
                fields
                    .as_object()
                    .into_iter()
                    .flat_map(|object| object.keys())
            })
            .map(|key| key.to_lowercase())
            .collect();
        let missing: Vec<&str> = NEW_RECIPE_REQUIRED
            .iter()
            .filter(|field| !given.contains(&field.to_lowercase()))
            .copied()
            .collect();
        if !missing.is_empty() {
            bail!("{RECIPES}.{name}: new recipes need {}", missing.join(", "));
        }
        let mut base = template.clone();
        let none = json!({"IngredientName": "None", "Count": 0});
        for (field, value) in [
            ("FractionNum", json!(1)),
            ("FailRate1", json!(0.0)),
            ("FailRate2", json!(0.0)),
            ("CatalystCost", json!(0)),
            ("bHideInGame", json!(false)),
            ("ProductSequence", json!([])),
            ("UnstableSequence", json!([])),
            ("FailProduct1", none.clone()),
            ("FailProduct2", none.clone()),
            ("SideProduct", none),
        ] {
            base[field] = value;
        }
        let product = parts
            .iter()
            .rev()
            .find_map(|(_, fields)| fields.get("ProductInfo")?.get("IngredientName")?.as_str())
            .unwrap_or("None");
        // an alternate recipe if another one already makes the same item
        base["bAlternate"] = json!(rows
            .values()
            .any(|row| name_of(get(get(row, "ProductInfo"), "IngredientName")) == product));
        let row = self.merge_parts(row_struct, &base, parts, &format!("{RECIPES}.{name}"))?;
        let product_info = get(&row, "ProductInfo");
        if !product_info
            .get("IngredientName")
            .is_some_and(|v| v.is_string())
            || !product_info.get("Count").is_some_and(|v| v.is_number())
        {
            bail!("{RECIPES}.{name}.ProductInfo: give IngredientName and Count");
        }
        Ok(row)
    }
}

/// IDs for new rows. A new building gets the first free ID in the hundred of the building it copies
/// (2xx are active crafting machines); other tables count on from the highest ID.
struct IdAllocator {
    buildings: bool,
    used: BTreeSet<i64>,
}

impl IdAllocator {
    fn new(table: &str, rows: &IndexMap<String, Value>, parts: &IndexMap<String, Parts>) -> Self {
        let mut used: BTreeSet<i64> = rows
            .values()
            .filter_map(|row| get(row, "ID").as_i64())
            .collect();
        for row_parts in parts.values() {
            used.extend(
                row_parts
                    .iter()
                    .filter_map(|(_, fields)| fields.get("ID")?.as_i64()),
            );
        }
        IdAllocator {
            buildings: table == "DT_Buildings",
            used,
        }
    }

    fn next(&mut self, source_id: i64, at: &str) -> Result<i64> {
        let found = if self.buildings {
            let group = source_id / 100 * 100;
            // the game starts each group at x01
            let free = (group + 1..group + 100).find(|id| !self.used.contains(id));
            free.with_context(|| {
                format!(
                    "{at}: no free building ID left in {group}-{}; give an \"ID\"",
                    group + 99
                )
            })?
        } else {
            self.used.last().copied().unwrap_or(0) + 1
        };
        self.used.insert(found);
        Ok(found)
    }
}

fn rename_values(value: &mut Value, renamed: &HashMap<String, String>) {
    match value {
        Value::String(string) => {
            if let Some(new) = renamed.get(string.as_str()) {
                *string = new.clone();
            }
        }
        Value::Array(elements) => elements.iter_mut().for_each(|e| rename_values(e, renamed)),
        Value::Object(object) => object.values_mut().for_each(|v| rename_values(v, renamed)),
        _ => {}
    }
}

/// Leaf values that differ between two rows, as `path = value` (whole lists as their length).
fn differences(old: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in b {
                let old_value = a.get(key).unwrap_or(&Value::Null);
                differences(old_value, value, &format!("{path}.{key}"), out);
            }
        }
        (Value::Array(a), Value::Array(b)) if a != b => {
            out.push(format!("{path} = list of {} entries", b.len()));
        }
        _ if old != new => out.push(format!("{path} = {new}")),
        _ => {}
    }
}

/// Apply edit files to the game's tables.
pub fn build(game: &Game, sources: &[Source]) -> Result<Built> {
    let mut editor = Editor {
        game,
        items: IndexMap::new(),
        tables: HashMap::new(),
        errors: vec![],
        warnings: skipped(sources),
    };
    let item_names = editor.display_names("DT_Enemies");
    for (id, row) in editor.rows("DT_Enemies")?.clone() {
        let display = item_names.get(&id).cloned().unwrap_or_else(|| id.clone());
        editor.items.insert(
            id,
            Item {
                display,
                hidden: flag(&row, "bHideInGame"),
                fluid: flag(&row, "IsLiquid"),
            },
        );
    }

    let edits = collect(sources)?;
    let mut order: Vec<&String> = edits.keys().collect();
    order.sort_by_key(|table| (table.as_str() != RECIPES, table.as_str().to_string()));

    let mut written: IndexMap<String, (String, Table)> = IndexMap::new(); // table -> (path, table)
    let mut new_rows = vec![];
    let mut renamed: HashMap<String, String> = HashMap::new(); // new recipe -> unused row it uses
    let mut changed: IndexMap<String, IndexMap<String, Value>> = IndexMap::new();
    let mut report = vec![];

    for table_name in order {
        let rows = &edits[table_name];
        let Some(path) = editor.asset_path(table_name) else {
            editor.errors.push(format!("{table_name}: unknown table"));
            continue;
        };
        let mut table = Table::open_asset(game.assets, &path, game.schemas)
            .with_context(|| format!("opening {table_name}"))?;
        let original = table.rows.clone();
        let row_struct = table.row_struct.clone();
        let mut slots = if table_name == RECIPES {
            editor.free_recipe_slots(&original)
        } else {
            vec![]
        };
        slots.retain(|slot| !rows.contains_key(slot)); // rows edited directly aren't free
        let mut ids = IdAllocator::new(table_name, &original, rows);

        for (row_name, parts) in rows {
            let at = format!("{table_name}.{row_name}");
            let result = (|| -> Result<(String, String, Value, bool)> {
                if is_true(option(parts, "_unlocked")) {
                    if table_name != RECIPES {
                        bail!("{at}: _unlocked only applies to recipes");
                    }
                    bail!(
                        "{at}: _unlocked needs a UE4SS mod (a .pak runs no code); unlock the \
                        recipe \
                         with a research instead (MODDING.md)"
                    );
                }
                if let Some(base) = original.get(row_name) {
                    let row = editor.merge_parts(&row_struct, base, parts, &at)?;
                    return Ok((row_name.clone(), row_name.clone(), row, false));
                }
                if let Some(source) = option(parts, "_copy") {
                    let source = source
                        .as_str()
                        .with_context(|| format!("{at}: _copy names a row"))?;
                    let base = original
                        .get(source)
                        .with_context(|| format!("{at}: _copy row '{source}' not found"))?;
                    let mut row = editor.merge_parts(&row_struct, base, parts, &at)?;
                    if row.get("ID").is_some() && option(parts, "ID").is_none() {
                        let source_id = get(base, "ID").as_i64().unwrap_or(0);
                        row["ID"] = json!(ids.next(source_id, &at)?);
                    }
                    return Ok((row_name.clone(), source.to_string(), row, true));
                }
                if table_name == RECIPES {
                    if let Some(slot) = option(parts, "_slot") {
                        let slot = slot.as_str().unwrap_or_default().to_string();
                        if !slots.contains(&slot) {
                            bail!(
                                "{at}: _slot '{slot}' is not a free row (free: {})",
                                slots.join(", ")
                            );
                        }
                        slots.retain(|free| *free != slot);
                        let row = editor.new_recipe(
                            row_name,
                            parts,
                            &original[&slot],
                            &table.rows,
                            &row_struct,
                        )?;
                        return Ok((slot.clone(), slot, row, false));
                    }
                    let craft = option(parts, "CraftType");
                    let template = original
                        .iter()
                        .find(|(_, row)| {
                            Some(get(row, "CraftType")) == craft && !flag(row, "bHideInGame")
                        })
                        .or_else(|| original.first())
                        .map(|(name, _)| name.clone())
                        .context("the recipe table is empty")?;
                    let mut row = editor.new_recipe(
                        row_name,
                        parts,
                        &original[&template],
                        &table.rows,
                        &row_struct,
                    )?;
                    if option(parts, "ID").is_none() {
                        let template_id = get(&original[&template], "ID").as_i64().unwrap_or(0);
                        row["ID"] = json!(ids.next(template_id, &at)?);
                    }
                    return Ok((row_name.clone(), template, row, true));
                }
                bail!("{at}: row not found (to add a row, give \"_copy\": \"<existing row>\")")
            })();
            match result {
                Ok((game_row, source, row, is_new)) => {
                    if game_row != *row_name {
                        renamed.insert(row_name.clone(), game_row.clone());
                        report.push(format!(
                            "new recipe {row_name} uses the unused game row {game_row}"
                        ));
                    }
                    if is_new {
                        new_rows.push((table_name.clone(), row_name.clone(), source.clone()));
                        report.push(format!(
                            "new row {table_name}.{row_name} (starting from {source})"
                        ));
                        table.add_row(&game_row, row, &source)?;
                    } else {
                        let mut lines = vec![];
                        differences(
                            &original[&game_row],
                            &row,
                            &format!("{table_name}.{game_row}"),
                            &mut lines,
                        );
                        report.extend(lines);
                        table.rows.insert(game_row.clone(), row);
                    }
                    changed
                        .entry(table_name.clone())
                        .or_default()
                        .insert(game_row, Value::Null);
                }
                Err(error) => editor.errors.push(format!("{error:#}")),
            }
        }
        written.insert(table_name.clone(), (path, table));
    }

    // Other tables refer to a new recipe by its own name; in the game it's the reused row
    if !renamed.is_empty() {
        for (table_name, (_, table)) in written.iter_mut() {
            if table_name == RECIPES {
                continue;
            }
            for name in changed
                .get(table_name)
                .map(|rows| rows.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if let Some(row) = table.rows.get_mut(&name) {
                    rename_values(row, &renamed);
                }
            }
        }
    }
    for (table_name, rows) in changed.iter_mut() {
        let table = &written[table_name].1;
        for (name, row) in rows.iter_mut() {
            *row = table.rows[name].clone();
        }
    }
    for (table_name, (_, table)) in &written {
        editor.tables.insert(table_name.clone(), table.rows.clone());
    }

    let (errors, warnings) = checks(&mut editor, &changed, &new_rows)?;
    editor.errors.extend(errors);
    editor.warnings.extend(warnings);
    if changed.contains_key(RECIPES) {
        let recipe_warnings = recipe_warnings(&mut editor, &changed[RECIPES])?;
        editor.warnings.extend(recipe_warnings);
    }

    let mut tables = vec![];
    let mut references = IndexMap::new();
    for (table_name, (path, table)) in &written {
        if !changed.contains_key(table_name) {
            continue;
        }
        match table.write() {
            Ok(result) => {
                for (reference, class) in result.references {
                    references.insert(reference, class);
                }
                tables.push(LegacyAsset {
                    path: path.clone(),
                    uasset: result.uasset,
                    uexp: result.uexp,
                    ubulk: None,
                });
            }
            Err(error) => editor.errors.push(format!("{table_name}: {error:#}")),
        }
    }
    // A reference to a game asset must name one that exists
    if let Some(packages) = game.packages {
        for reference in references.keys() {
            let package = reference.split('.').next().unwrap_or(reference);
            if !reference.starts_with("/Game/Mods/") && !packages.contains(package) {
                editor
                    .errors
                    .push(format!("{reference} isn't an asset in the game"));
            }
        }
    }
    for (reference, class) in &references {
        report.push(format!(
            "new reference {reference} ({})",
            class.rsplit('.').next().unwrap_or(class)
        ));
    }
    if !editor.errors.is_empty() {
        bail!("{}", editor.errors.join("\n"));
    }
    Ok(Built {
        tables,
        new_rows,
        changed,
        references,
        warnings: editor.warnings,
        report,
    })
}

/// Mistakes found in testing: (errors, warnings).
fn checks(
    editor: &mut Editor,
    changed: &IndexMap<String, IndexMap<String, Value>>,
    new_rows: &[(String, String, String)],
) -> Result<(Vec<String>, Vec<String>)> {
    let (mut errors, mut warnings) = (vec![], vec![]);
    let game_buildings: IndexMap<String, Value> = {
        let path = editor
            .asset_path("DT_Buildings")
            .context("no DT_Buildings")?;
        let decoded = read_datatable(editor.game.assets.asset(&path)?, editor.game.schemas, None)?;
        decoded["rows"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };

    // IDs and research slots shared with another row
    for (table_name, rows) in changed {
        let all = editor.rows(table_name)?.clone();
        let mut by_id: IndexMap<i64, Vec<&String>> = IndexMap::new();
        for (name, row) in &all {
            if let Some(id) = get(row, "ID").as_i64() {
                by_id.entry(id).or_default().push(name);
            }
        }
        for (id, names) in by_id {
            if names.len() > 1 && names.iter().any(|name| rows.contains_key(*name)) {
                let names: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
                errors.push(format!(
                    "{table_name}: rows {} share ID {id}",
                    names.join(", ")
                ));
            }
        }
        if table_name == "DT_SkillPoints" {
            let mut by_slot: IndexMap<(i64, i64), Vec<&String>> = IndexMap::new();
            for (name, row) in &all {
                if !flag(row, "Deprecated") {
                    let slot = get(row, "SlotPosition");
                    let position = (
                        get(slot, "X").as_i64().unwrap_or(0),
                        get(slot, "Y").as_i64().unwrap_or(0),
                    );
                    by_slot.entry(position).or_default().push(name);
                }
            }
            for (slot, names) in by_slot {
                if names.len() > 1 && names.iter().any(|name| rows.contains_key(*name)) {
                    let names: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
                    errors.push(format!(
                        "DT_SkillPoints: research {} share the slot {slot:?}",
                        names.join(", ")
                    ));
                }
            }
        }
    }

    let skills = editor.rows("DT_SkillPoints")?.clone();
    let buildings = editor.rows("DT_Buildings")?.clone();
    let y = |row: &Value| get(get(row, "SlotPosition"), "Y").as_i64().unwrap_or(0);
    // Research: the tree draws links downwards, and every game research is below its predecessors
    for name in changed
        .get("DT_SkillPoints")
        .map(|rows| rows.keys().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let row = &skills[name];
        if flag(row, "Deprecated") {
            continue;
        }
        for before in get(row, "Predecessors").as_array().into_iter().flatten() {
            let before = name_of(before);
            match skills.get(before) {
                Some(predecessor) if y(predecessor) >= y(row) => warnings.push(format!(
                    "DT_SkillPoints.{name}: at row {}, not below its predecessor {before} (row \
                    {}), so the \
                     link to it is drawn through other nodes; give it a larger SlotPosition.Y",
                    y(row),
                    y(predecessor)
                )),
                None => warnings.push(format!(
                    "DT_SkillPoints.{name}: unknown predecessor \
                    {before}"
                )),
                _ => {}
            }
        }
    }

    let unlocked_by = |name: &str, config_type: &str| -> (Vec<String>, Vec<String>) {
        let (mut usable, mut other) = (vec![], vec![]);
        for (key, row) in &skills {
            let item = get(row, "UnlockItem");
            let unlocks = (name_of(get(item, "ConfigType")) == config_type
                && name_of(get(item, "ConfigName")) == name)
                || (config_type == "ConstructOptions"
                    && get(row, "ExtraUnlockConstructions")
                        .as_array()
                        .is_some_and(|extra| {
                            extra.iter().any(|value| value.as_str() == Some(name))
                        }));
            if unlocks {
                if flag(row, "Deprecated") || flag(row, "LevelSkill") {
                    other.push(key.clone());
                } else {
                    usable.push(key.clone());
                }
            }
        }
        (usable, other)
    };

    for (name, row) in changed.get("DT_Buildings").into_iter().flatten() {
        let alternative = name_of(get(row, "AlterBuildOption"));
        if alternative != "None" && !buildings.contains_key(alternative) {
            errors.push(format!(
                "DT_Buildings.{name}: AlterBuildOption {alternative} is not a building"
            ));
        }
    }
    let mut mentioned_cache: Option<HashSet<String>> = None;
    for (table_name, name, source) in new_rows {
        if table_name != "DT_Buildings" {
            continue;
        }
        let (row, original) = (&buildings[name], &game_buildings[source]);
        let alternative = name_of(get(row, "AlterBuildOption"));
        if alternative != "None" && alternative == name_of(get(original, "AlterBuildOption")) {
            warnings.push(format!(
                "DT_Buildings.{name}: AlterBuildOption is still {source}'s mirrored variant \
                {alternative}, so \
                 mirroring it builds that; set \"None\" (or a variant of its own)"
            ));
        }
        let (id, source_id) = (
            get(row, "ID").as_i64().unwrap_or(0),
            get(original, "ID").as_i64().unwrap_or(0),
        );
        if id / 100 != source_id / 100 {
            let group = source_id / 100 * 100;
            warnings.push(format!(
                "DT_Buildings.{name}: ID {id} is outside the group of {source} ({}-{}), which \
                sorts buildings \
                 by kind",
                group + 1,
                group + 99
            ));
        }
        let (usable, other) = unlocked_by(name, "ConstructOptions");
        if usable.is_empty() {
            if !other.is_empty() {
                warnings.push(format!(
                    "DT_Buildings.{name}: only unlocked by {}, which is deprecated or a level \
                    research \
                     (whose UnlockItem didn't work in testing)",
                    other.join(", ")
                ));
            } else {
                if mentioned_cache.is_none() {
                    mentioned_cache = Some(strings_in_other_tables(editor)?);
                }
                if !mentioned_cache.as_ref().unwrap().contains(name) {
                    warnings.push(format!(
                        "DT_Buildings.{name}: nothing unlocks it (add a DT_SkillPoints row with \
                         \"UnlockItem\": {{\"ConfigName\": \"{name}\", \"ConfigType\": \
                         \"ConstructOptions\"}})"
                    ));
                }
            }
        }
    }
    for (table_name, name, _) in new_rows {
        if table_name != RECIPES {
            continue;
        }
        let (usable, other) = unlocked_by(name, "CraftingRecipes");
        if usable.is_empty() {
            warnings.push(if other.is_empty() {
                format!(
                    "{RECIPES}.{name}: no research unlocks it (MODDING.md, \"Unlocking new \
                    recipes with research\")"
                )
            } else {
                format!(
                    "{RECIPES}.{name}: only unlocked by {}, which is deprecated or a level \
                    research",
                    other.join(", ")
                )
            });
        }
    }
    Ok((errors, warnings))
}

/// Every string in the tables other than DT_Buildings and DT_SkillPoints (workbench, quests,
/// licences, ...), to tell whether something refers to a building.
fn strings_in_other_tables(editor: &mut Editor) -> Result<HashSet<String>> {
    fn walk(value: &Value, out: &mut HashSet<String>) {
        match value {
            Value::String(string) => drop(out.insert(string.clone())),
            Value::Array(elements) => elements.iter().for_each(|e| walk(e, out)),
            Value::Object(object) => object.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    let names: Vec<String> = editor
        .game
        .assets
        .of_class(&["DataTable"])
        .iter()
        .filter_map(|path| path.rsplit('/').next().map(str::to_string))
        .filter(|name| {
            name.starts_with("DT_") && name != "DT_Buildings" && name != "DT_SkillPoints"
        })
        .collect();
    for name in names {
        if let Ok(rows) = editor.rows(&name) {
            rows.values().for_each(|row| walk(row, &mut out));
        }
    }
    Ok(out)
}

/// A building's belt and pipe connections: (belt in, belt out, pipe in, pipe out).
fn ports(building: &Value) -> [i64; 4] {
    let mut out = [0; 4];
    for connection in get(building, "InOutList").as_array().into_iter().flatten() {
        if flag(connection, "IsPipeGrid") {
            let direction = name_of(get(connection, "PipeOverrideType"));
            out[2] += (direction != "OnlyOutput") as i64;
            out[3] += (direction != "OnlyInput") as i64;
            continue;
        }
        for side in ["Right", "Bottom", "Left", "Up"] {
            out[0] += flag(get(connection, side), "IsInput") as i64;
            out[1] += flag(get(connection, side), "IsOutput") as i64;
        }
    }
    out
}

/// Recipe edits the machines probably can't handle, judged from the game's own recipes.
fn recipe_warnings(editor: &mut Editor, rows: &IndexMap<String, Value>) -> Result<Vec<String>> {
    let game_recipes: IndexMap<String, Value> = {
        let path = editor.asset_path(RECIPES).context("no recipes table")?;
        let decoded = read_datatable(editor.game.assets.asset(&path)?, editor.game.schemas, None)?;
        decoded["rows"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let buildings = editor.rows("DT_Buildings")?.clone();
    let names = editor.display_names("DT_Buildings");
    let craft_type = |row: &Value| match get(row, "CraftType") {
        Value::String(name) => name.clone(),
        other => other.to_string(),
    };
    // output field -> the craft types with a game recipe that uses it
    let used: HashMap<&str, HashSet<String>> = OUTPUT_FIELDS
        .iter()
        .map(|field| {
            let types = game_recipes
                .values()
                .filter(|row| name_of(get(get(row, field), "IngredientName")) != "None")
                .map(craft_type)
                .collect();
            (*field, types)
        })
        .collect();
    let fluid = |item: &str| editor.items.get(item).is_some_and(|item| item.fluid);
    let mut out = vec![];
    for (name, row) in rows {
        let kind = craft_type(row);
        let machines: Vec<(&str, &Value)> = machines_for(&kind)
            .iter()
            .filter_map(|machine| buildings.get(*machine).map(|row| (*machine, row)))
            .collect();
        let listed: Vec<String> = machines
            .iter()
            .map(|(machine, _)| {
                names
                    .get(*machine)
                    .cloned()
                    .unwrap_or_else(|| machine.to_string())
            })
            .collect();
        let machine_names = if listed.is_empty() {
            kind.clone()
        } else {
            listed.join(", ")
        };
        for field in OUTPUT_FIELDS {
            let item = name_of(get(get(row, field), "IngredientName"));
            if item != "None" && !used[field].contains(&kind) {
                out.push(format!(
                    "{name}: {field} {item} - no {machine_names} recipe in the game has a {field}, \
                    so the \
                     machine probably discards it"
                ));
            }
        }
        // Belt and pipe connections: solid items travel on belts, fluids in pipes
        let mut needs: BTreeSet<(bool, bool)> = BTreeSet::new(); // (is output, is fluid)
        for ingredient in get(row, "IngredientList").as_array().into_iter().flatten() {
            needs.insert((false, fluid(name_of(get(ingredient, "IngredientName")))));
        }
        let shops = machines.iter().all(|(_, building)| {
            get(building, "BuildingTags")
                .as_array()
                .is_some_and(|tags| {
                    tags.iter()
                        .any(|tag| tag.as_str() == Some("Building.Shop.Shelves"))
                })
        });
        if !shops {
            // shops sell their output
            for field in ["ProductInfo"].iter().chain(OUTPUT_FIELDS) {
                let item = name_of(get(get(row, field), "IngredientName"));
                if item != "None" {
                    needs.insert((true, fluid(item)));
                }
            }
        }
        for (is_output, is_fluid) in needs {
            let index = (is_fluid as usize) * 2 + is_output as usize;
            if !machines.is_empty()
                && machines
                    .iter()
                    .all(|(_, building)| ports(building)[index] == 0)
            {
                let (kind_name, direction) = (
                    if is_fluid { "pipe" } else { "belt" },
                    if is_output { "out" } else { "in" },
                );
                let what = match (is_fluid, is_output) {
                    (true, false) => "fluid ingredients",
                    (true, true) => "fluid outputs",
                    (false, false) => "solid item ingredients",
                    (false, true) => "solid item outputs",
                };
                out.push(format!(
                    "{name}: {machine_names} has no {kind_name} {direction}puts, so \
                    it can't take {what}"
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skipped_edits_are_reported_but_comments_are_not() {
        let source = Source {
            label: "Example/mine.json".into(),
            contents: json!({
                "_comment": "notes",
                "_DT_Quests": {"Main2": {"RequireProducts": {"Nums": [25]}}},
                "_DT_Empty": {"_comment": "only a comment"},
                "DT_EnemyCrafting": {
                    "_comment": "rows",
                    "_WoodGear": {"ProductInfo": {"Count": 5}},
                    "LinseedOil": {"_comment": "x", "ProductInfo": {"Count": 30}}
                }
            }),
        };
        let found = skipped(&[source]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("_DT_Quests is skipped"));
        assert!(found[1].contains("DT_EnemyCrafting._WoodGear is skipped"));
    }
}
