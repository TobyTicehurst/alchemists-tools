//! A mod folder's mod.json, a built mod's entry in the install record, and the game's build.

use super::*;

/// A mod as built and installed: the `build.json` of its build, and its entry in the install
/// record (the Python tool's form).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Entry {
    pub name: String,
    pub title: String,
    pub version: String,
    pub game_build: Option<String>,
    pub dependencies: Vec<String>,
    /// Files it installs, relative to the game folder.
    pub files: Vec<String>,
    /// Its own data container and template save: for the zip only (installs merge them).
    pub data_files: Vec<String>,
    pub tables: Vec<String>,
    pub new_buildings: Vec<String>,
    pub needs_ue4ss: bool,
    /// Game functions its C++ code detours (as its mod.json declares): no two mods may detour one.
    pub detours: Vec<String>,
    /// Game functions it hooks through UE4SS, as found in its Lua (`RegisterHook`) and DLL.
    pub hooks: Vec<String>,
    /// What couldn't be checked about it, warned about whenever it's installed (an imported mod's:
    /// see [`crate::import`]).
    pub cautions: Vec<String>,
    /// Its new craft types: `Mod:Name` -> number.
    pub craft_type_numbers: IndexMap<String, u8>,
    /// Its edit files (craft type names already numbers): (label, contents).
    pub edits: Vec<(String, Value)>,
    pub optional_edits: IndexMap<String, Vec<(String, Value)>>,
    pub save_edits: Vec<SaveEdit>,
    /// The cooked assets it packs: object path -> class path.
    pub assets: IndexMap<String, String>,
    /// The rows it adds: (table, row).
    pub new_rows: Vec<(String, String)>,
}

impl Entry {
    pub(super) fn requires(&self) -> Result<Vec<Dependency>> {
        self.dependencies
            .iter()
            .map(|text| parse_dependency(text, &self.name))
            .collect()
    }
    pub(super) fn version(&self) -> Result<Version> {
        parse_version(&self.version, &self.name)
    }
}

/// A mod folder, read and checked.
pub struct Mod {
    pub folder: PathBuf,
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub contact: String,
    pub homepage: String,
    /// The Thunderstore team that publishes it (see [`crate::sharing`]).
    pub team: String,
    pub dependencies: Vec<String>,
    pub requires: Vec<Dependency>,
    /// The game's Steam build it's made for, if mod.json says.
    pub game_build: Option<String>,
    /// The game functions its C++ code detours, if mod.json says (None: it doesn't say).
    pub detours: Option<Vec<String>>,
    pub content: Vec<String>,
    /// Blueprint asset -> craft type (a name, a number, or `Mod:Name`).
    pub craft_types: IndexMap<String, Value>,
    pub new_craft_types: Vec<String>,
    pub edits: Vec<(String, Value)>,
    pub optional_edits: IndexMap<String, Vec<(String, Value)>>,
    pub save_edits: Vec<SaveEdit>,
}

pub(super) fn read_json(path: &Path) -> Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", display_path(path)))?;
    serde_json::from_str(&text).with_context(|| format!("{} isn't valid JSON", display_path(path)))
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("writing {}", display_path(path)))
}

/// `*.json` in a folder, sorted, as (label `<prefix>/<file>`, contents).
fn read_edits(folder: &Path, prefix: &str) -> Result<Vec<(String, Value)>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            Ok((
                format!("{prefix}/{}", path.file_name().unwrap().to_string_lossy()),
                read_json(&path)?,
            ))
        })
        .collect()
}

fn string_list(value: Option<&Value>, at: &str, field: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(vec![]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .with_context(|| format!("{at}: {field} lists strings"))
            })
            .collect(),
        Some(_) => bail!("{at}: {field} is a list"),
    }
}

impl Mod {
    pub fn load(folder: &Path) -> Result<Self> {
        let path = folder.join("mod.json");
        let at = display_path(&path);
        let raw = read_json(&path)?;
        let fields = raw
            .as_object()
            .with_context(|| format!("{at}: mod.json is an object"))?;
        let unknown: Vec<&String> = fields
            .keys()
            .filter(|key| !key.starts_with('_') && !MOD_FIELDS.contains(&key.as_str()))
            .collect();
        if !unknown.is_empty() {
            let unknown: Vec<&str> = unknown.iter().map(|key| key.as_str()).collect();
            let mut known = MOD_FIELDS.to_vec();
            known.sort();
            bail!(
                "{at}: unknown field(s) {} (fields: {})",
                unknown.join(", "),
                known.join(", ")
            );
        }
        let text = |key: &str, default: &str| {
            fields
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_string()
        };
        let name = text("name", "");
        if !is_name(&name) || name == MERGED || name == MERGED_SAVE {
            bail!(
                "{at}: name must be letters and digits, starting with a letter (it names files and \
                folders), \
                 and not {MERGED_SAVE} or {MERGED}"
            );
        }
        let version = text("version", "0.1.0");
        parse_version(&version, &at)?;
        let content = string_list(fields.get("content"), &at, "content")?;
        for path in &content {
            if !path.starts_with(&format!("/Game/Mods/{name}")) {
                bail!(
                    "{at}: content path {path} must be in /Game/Mods/{name}/, so that no two \
                    mods share asset paths"
                );
            }
        }
        let new_craft_types = string_list(fields.get("new-craft-types"), &at, "new-craft-types")?;
        if let Some(bad) = new_craft_types
            .iter()
            .find(|craft_type| !is_name(craft_type))
        {
            bail!("{at}: new craft type {bad:?} must be letters and digits");
        }
        let dependencies = string_list(fields.get("dependencies"), &at, "dependencies")?;
        let requires: Vec<Dependency> = dependencies
            .iter()
            .map(|text| parse_dependency(text, &at))
            .collect::<Result<_>>()?;
        let names: Vec<&str> = requires
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect();
        if names.contains(&name.as_str())
            || names.iter().collect::<HashSet<_>>().len() != names.len()
        {
            bail!("{at}: a mod can't depend on itself, or name the same mod twice");
        }
        let game_build = match fields.get("game-build") {
            None => None,
            Some(Value::String(build)) if is_build(build) => Some(build.clone()),
            Some(other) => bail!(
                "{at}: game-build {other} must be the game's Steam build number, as a string (e.g. \
                 \"25991404\"; alchemists-tools check shows the installed one)"
            ),
        };
        let detours = match fields.get("detours") {
            None => None,
            listed => Some(string_list(listed, &at, "detours")?),
        };
        let team = text("team", "");
        if !team.is_empty() && !crate::sharing::is_team(&team) {
            bail!(
                "{at}: team {team:?} must be a Thunderstore team name: letters, digits and _, not \
                 starting or ending with _"
            );
        }
        let craft_types = fields
            .get("craft-types")
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let edits_folder = folder.join("edits");
        let edits = read_edits(&edits_folder, &name)?;
        let mut optional_edits = IndexMap::new();
        let mut subfolders: Vec<PathBuf> = std::fs::read_dir(&edits_folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .is_some_and(|file_name| file_name.to_string_lossy().starts_with("if-"))
            })
            .collect();
        subfolders.sort();
        for sub in subfolders {
            let other = sub.file_name().unwrap().to_string_lossy()["if-".len()..].to_string();
            let dependency = requires.iter().find(|dependency| dependency.name == other);
            if dependency.is_none_or(|dependency| {
                dependency.kind.is_required() || dependency.kind == Kind::Incompatible
            }) {
                bail!(
                    "{}: edits for when {other} is installed need {other} as an optional \
                    dependency \
                     (\"? {other}\", \"(?) {other}\" or \"+ {other}\" in mod.json)",
                    display_path(&sub)
                );
            }
            optional_edits.insert(
                other.clone(),
                read_edits(&sub, &format!("{name}/if-{other}"))?,
            );
        }

        let mut save_edits = vec![];
        let saves = folder.join("save");
        if saves.is_dir() {
            for entry in std::fs::read_dir(&saves)?.flatten() {
                if entry.file_name() != "DefaultGame.json" {
                    bail!(
                        "{}: only DefaultGame.json (the template save new games start from) can be \
                        edited",
                        display_path(&entry.path())
                    );
                }
            }
            let file = saves.join("DefaultGame.json");
            let listed = read_json(&file)?;
            for edit in listed
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let object = edit.as_object().filter(|object| {
                    object
                        .keys()
                        .all(|key| key.starts_with('_') || SAVE_EDIT_FIELDS.contains(&key.as_str()))
                        && object.contains_key("property")
                        && object.contains_key("value")
                });
                let Some(object) = object else {
                    bail!(
                        "{}: each edit has \"property\" and \"value\", and optionally \
                        \"occurrence\", \
                         \"after\", \"expect\" and \"in\" (got {edit})",
                        display_path(&file)
                    );
                };
                let cleaned: serde_json::Map<String, Value> = object
                    .iter()
                    .filter(|(key, _)| !key.starts_with('_'))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                save_edits.push(serde_json::from_value(Value::Object(cleaned))?);
            }
        }
        Ok(Mod {
            folder: folder.to_path_buf(),
            title: text("title", &name),
            description: text("description", ""),
            author: text("author", ""),
            contact: text("contact", ""),
            homepage: text("homepage", ""),
            team,
            name,
            version,
            dependencies,
            requires,
            game_build,
            detours,
            content,
            craft_types,
            new_craft_types,
            edits,
            optional_edits,
            save_edits,
        })
    }
}

impl Mod {
    /// Its picture: `thumbnail.png` (or `.jpg`) in its folder.
    pub fn thumbnail(&self) -> Option<PathBuf> {
        ["thumbnail.png", "thumbnail.jpg"]
            .iter()
            .map(|file| self.folder.join(file))
            .find(|path| path.is_file())
    }
}

fn is_build(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether a mod made for game build `made_for` is older than the game's build `current`.
pub fn older_build(made_for: &str, current: &str) -> bool {
    match (made_for.parse::<u64>(), current.parse::<u64>()) {
        (Ok(made_for), Ok(current)) => made_for < current,
        _ => false,
    }
}

/// The install record's entry for a mod known only from its manifest (one installed from its zip).
pub(super) fn entry_from_manifest(name: &str, manifest: &Manifest) -> Entry {
    Entry {
        name: name.to_string(),
        title: manifest.title.clone(),
        version: manifest.version.clone(),
        game_build: manifest.game_build.clone(),
        dependencies: manifest.dependencies.clone(),
        files: manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect(),
        tables: manifest.tables.clone(),
        new_buildings: manifest.new_buildings.clone(),
        detours: manifest.detours.clone(),
        hooks: manifest.hooks.clone(),
        cautions: manifest.cautions.clone(),
        ..Entry::default()
    }
}

/// The install record of a game folder: the mods installed in it (empty if none are).
pub fn installed_record(game: &Game) -> Result<Record> {
    let path = join(&game.root, PAK_FOLDER).join(RECORD);
    if !path.exists() {
        return Ok(Record::default());
    }
    Ok(serde_json::from_value(read_json(&path)?)?)
}

/// Stop if the game is running: it holds the files mods replace.
pub(crate) fn check_game_closed() -> Result<()> {
    if crate::environment::game_running() {
        bail!("the game is running: close it first (it holds the files mods replace)");
    }
    Ok(())
}

pub(super) fn data_container(name: &str) -> String {
    format!("zzz_{name}_P")
}

pub(super) fn content_container(name: &str) -> String {
    format!("zzz_{name}Content_P")
}

pub(super) fn save_container(name: &str) -> String {
    format!("zzz_{name}Save_P")
}

/// The changes Steam made last: the game's build number, from its app manifest.
pub fn game_build(game: &Game) -> Option<String> {
    let steamapps = game.root.parent()?.parent()?;
    let text =
        std::fs::read_to_string(steamapps.join(format!("appmanifest_{STEAM_APP}.acf"))).ok()?;
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("\"buildid\""))?;
    line.split('"').nth(3).map(str::to_string)
}
