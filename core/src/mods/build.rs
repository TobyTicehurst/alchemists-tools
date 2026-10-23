//! Building a mod into `<build>/<name>/`.

use super::*;

pub(super) fn leaf_values(
    value: &Value,
    path: Vec<String>,
    collected: &mut Vec<(Vec<String>, Value)>,
) {
    match value {
        Value::Object(object) => {
            for (key, inner) in object {
                if !key.starts_with('_') {
                    let mut next = path.clone();
                    next.push(key.clone());
                    leaf_values(inner, next, collected);
                }
            }
        }
        other => collected.push((path, other.clone())),
    }
}

pub(crate) fn copy_folder(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Every file below `folder`, as paths relative to it with `/`.
pub(super) fn files_below(folder: &Path) -> Vec<String> {
    let mut files = vec![];
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(folder) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    files
}

/// The modules a Lua script loads with `require("Name")` (or `require "Name"`).
fn required_modules(script: &str) -> Vec<String> {
    string_arguments(script, "require")
}

/// The text each call of `function` in a Lua script is given first, when that's a string:
/// `function("text", ...)` or `function "text"`.
pub(crate) fn string_arguments(script: &str, function: &str) -> Vec<String> {
    let mut modules = vec![];
    let mut remaining = script;
    while let Some(at) = remaining.find(function) {
        remaining = &remaining[at + function.len()..];
        let after = remaining.trim_start().trim_start_matches('(').trim_start();
        if let Some(quote) = after
            .chars()
            .next()
            .filter(|character| *character == '"' || *character == '\'')
        {
            if let Some(end) = after[1..].find(quote) {
                modules.push(after[1..1 + end].to_string());
            }
        }
    }
    modules
}

/// The reflected functions (`/Script/Module.Class:Function`) a DLL names, as UTF-16 (C++'s
/// `STR("...")`) or as bytes.
pub(crate) fn function_paths(dll: &[u8]) -> Vec<String> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"/Script/[A-Za-z0-9_]+\.[A-Za-z0-9_]+:[A-Za-z0-9_]+").unwrap()
    });
    let mut texts = vec![String::from_utf8_lossy(dll).into_owned()];
    for start in 0..2 {
        let units: Vec<u16> = dll[start.min(dll.len())..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        texts.push(String::from_utf16_lossy(&units));
    }
    let found: BTreeSet<String> = texts
        .iter()
        .flat_map(|text| {
            pattern
                .find_iter(text)
                .map(|found| found.as_str().to_string())
        })
        .collect();
    found.into_iter().collect()
}

/// The engine's own functions, which many mods hook (to start when a player spawns, say), so
/// sharing one isn't worth a warning.
pub(super) fn is_engine_function(path: &str) -> bool {
    ["/Script/Engine.", "/Script/CoreUObject.", "/Script/UMG."]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

impl Workspace {
    /// Build a mod into `<build>/<name>/`: its tables (on top of the mods it requires), its cooked
    /// content, its template save, and its UE4SS files; then check them against each other. Its
    /// warnings are reported as about it.
    pub fn build_mod(&self, mod_: &Mod) -> Result<(PathBuf, Entry)> {
        report::about(&mod_.name, || self.build(mod_))
    }

    fn build(&self, mod_: &Mod) -> Result<(PathBuf, Entry)> {
        let name = &mod_.name;
        let build_folder = self.build.join(name);
        let _ = std::fs::remove_dir_all(&build_folder);
        let paks = join(&build_folder, PAK_FOLDER);
        std::fs::create_dir_all(&paks)?;
        // first, so their craft types are known
        let needed = self.required_mods(name, &mod_.requires)?;
        let own_numbers = self.allocate_craft_types(name, &mod_.new_craft_types)?;
        let known = self.known_craft_types()?;
        let resolve = |files: &[(String, Value)]| -> Result<Vec<(String, Value)>> {
            files
                .iter()
                .map(|(label, data)| {
                    Ok((
                        label.clone(),
                        resolve_craft_types(data, mod_, &known, label)?,
                    ))
                })
                .collect()
        };
        let own_edits = resolve(&mod_.edits)?;
        let optional: IndexMap<String, Vec<(String, Value)>> = mod_
            .optional_edits
            .iter()
            .map(|(key, files)| Ok((key.clone(), resolve(files)?)))
            .collect::<Result<_>>()?;
        // the game's layouts (the .usmap) are only read when the mod needs them: for its tables,
        // craft types or cooked content, not for a mod of only Lua or C++
        let mut machine_types: IndexMap<String, u8> = IndexMap::new();
        for (asset, value) in &mod_.craft_types {
            let value =
                craft_type_number(value, mod_, &known, &format!("{name}/mod.json craft-types"))?;
            let text = match &value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            machine_types.insert(
                asset.clone(),
                content::craft_type_number(&text, self.schemas()?)?,
            );
        }
        let current_build = game_build(&self.game);
        if let (Some(made_for), Some(current)) = (&mod_.game_build, &current_build) {
            if older_build(made_for, current) {
                warning!(
                    "made for game build {made_for}, and the game is build {current} now: check \
                     it still works, then set \"game-build\" in mod.json to {current}"
                );
            }
        }
        let mut info = Entry {
            name: name.clone(),
            title: mod_.title.clone(),
            version: mod_.version.clone(),
            game_build: current_build,
            dependencies: mod_.dependencies.clone(),
            detours: mod_.detours.clone().unwrap_or_default(),
            craft_type_numbers: own_numbers,
            edits: own_edits.clone(),
            optional_edits: optional.clone(),
            save_edits: mod_.save_edits.clone(),
            ..Entry::default()
        };

        let mut built = None;
        if !own_edits.is_empty() {
            let labels: Vec<&str> = own_edits.iter().map(|(label, _)| label.as_str()).collect();
            let on_top: Vec<&str> = needed.iter().map(|entry| entry.name.as_str()).collect();
            note!(
                "== table edits ({}){}",
                labels.join(", "),
                if on_top.is_empty() {
                    String::new()
                } else {
                    format!(", on top of {}", on_top.join(", "))
                }
            );
            let mut sources: Vec<edits::Source> = needed
                .iter()
                .flat_map(|entry| table_edits_of(entry, &HashSet::new()))
                .collect();
            sources.extend(own_edits.iter().map(|(label, contents)| edits::Source {
                label: label.clone(),
                contents: contents.clone(),
            }));
            let result = edits::build(&self.game_for_edits()?, &sources)?;
            let utoc = paks.join(format!("{}.utoc", data_container(name)));
            write_container(
                &utoc,
                &result.tables,
                retoc::version::EngineVersion::UE5_6,
                script_objects(&self.game)?,
            )?;
            check_container(&utoc, &result.tables, &self.game)?;
            let rows: usize = result.changed.values().map(|rows| rows.len()).sum();
            note!(
                ".pak mod: {rows} row(s) in {} table(s), checked",
                result.changed.len()
            );
            for line in &result.report {
                note!("   {line}");
            }
            for warning in &result.warnings {
                warning!("{warning}");
            }
            info.tables = result.changed.keys().cloned().collect();
            info.tables.sort();
            if !optional.is_empty() {
                let others: Vec<&str> = optional.keys().map(|name| name.as_str()).collect();
                note!(
                    "   edits for when {} are installed are checked when they're merged",
                    others.join(", ")
                );
            }
            built = Some(result);
        }

        if !mod_.content.is_empty() {
            note!("== cooked content ({})", mod_.content.join(", "));
            content::check_paths(&mod_.content, false)?;
            let stale: Vec<String> = mod_
                .content
                .iter()
                .flat_map(|path| content::stale_cooks(&self.project, path))
                .collect();
            if !stale.is_empty() {
                bail!(
                    "the cook is out of date for:\n   {}\n\
                     cook again (Platforms -> Windows -> Cook Content)",
                    stale.join("\n   ")
                );
            }
            let mut staged = content::stage(&self.project, &mod_.content)?;
            for (asset, &value) in &machine_types {
                let staged_asset = staged
                    .iter_mut()
                    .find(|staged| staged.path.rsplit('/').next() == Some(asset.as_str()))
                    .with_context(|| {
                        format!("craft type for {asset}: no {asset} among the cooked assets")
                    })?;
                let old = content::patch_craft_type(staged_asset, value)?;
                note!("   craft type {old} -> {value} in {asset}");
            }
            let utoc = paks.join(format!("{}.utoc", content_container(name)));
            let packaged =
                content::package(&utoc, &staged, &self.game, self.schemas()?, &machine_types)?;
            for (package, components) in &packaged.components {
                note!("   {package}:");
                for component in components {
                    note!("      {}", content::describe(component));
                }
            }
            info.assets = packaged.assets;
        }

        if !mod_.save_edits.is_empty() {
            note!("== template save (save/DefaultGame.json)");
            let edits: Vec<SaveEdit> = needed
                .iter()
                .flat_map(|entry| entry.save_edits.clone())
                .chain(mod_.save_edits.clone())
                .collect();
            self.write_save(&edits, &paks, &save_container(name))?;
        }

        let scripts = mod_.folder.join("ue4ss");
        if scripts.is_dir() {
            note!("== UE4SS mod ({})", display_path(&scripts));
            // A Lua mod has Scripts/main.lua, a C++ mod dlls/main.dll (UE4SS loads either from the folder)
            if !scripts.join("Scripts").join("main.lua").exists()
                && !scripts.join("dlls").join("main.dll").exists()
            {
                bail!(
                    "{}: a UE4SS mod needs Scripts/main.lua (Lua) or dlls/main.dll (C++)",
                    display_path(&scripts)
                );
            }
            let target = join(&build_folder, UE4SS_FOLDER).join(name);
            copy_folder(&scripts, &target)?;
            std::fs::write(target.join("enabled.txt"), "")?;
            info.needs_ue4ss = true;
        }
        let shared = mod_.folder.join("ue4ss-shared");
        if shared.is_dir() {
            note!("== UE4SS shared Lua modules ({})", display_path(&shared));
            for entry in std::fs::read_dir(&shared)?.flatten() {
                let module = entry.file_name().to_string_lossy().into_owned();
                if !module.starts_with(name.as_str()) {
                    bail!(
                        "{}: shared modules are named after their mod ({name}...), so no two mods \
                         install the same one",
                        display_path(&entry.path())
                    );
                }
                if !entry.path().join(format!("{module}.lua")).exists() {
                    bail!(
                        "{}: needs {module}.lua, which require(\"{module}\") loads",
                        display_path(&entry.path())
                    );
                }
                copy_folder(
                    &entry.path(),
                    &join(&build_folder, UE4SS_FOLDER)
                        .join("shared")
                        .join(&module),
                )?;
            }
            info.needs_ue4ss = true;
        }
        // A script that requires another mod's shared module needs that mod as a dependency
        let mut libraries: HashMap<String, String> = HashMap::new();
        for folder in self.mod_folders() {
            for module in std::fs::read_dir(folder.join("ue4ss-shared"))
                .into_iter()
                .flatten()
                .flatten()
            {
                libraries.insert(
                    module.file_name().to_string_lossy().into_owned(),
                    folder.file_name().unwrap().to_string_lossy().into_owned(),
                );
            }
        }
        let depends: HashSet<&str> = mod_
            .requires
            .iter()
            .filter(|dependency| dependency.kind != Kind::Incompatible)
            .map(|dependency| dependency.name.as_str())
            .chain([name.as_str()])
            .collect();
        // and the game functions it hooks: Lua's RegisterHook, and the functions its DLL names
        let mut hooks: BTreeSet<String> = BTreeSet::new();
        for folder in [&scripts, &shared] {
            for file in files_below(folder)
                .into_iter()
                .filter(|file| file.ends_with(".lua"))
            {
                let text = std::fs::read_to_string(folder.join(&file)).unwrap_or_default();
                hooks.extend(string_arguments(&text, "RegisterHook"));
                for module in required_modules(&text) {
                    if let Some(owner) = libraries
                        .get(&module)
                        .filter(|owner| !depends.contains(owner.as_str()))
                    {
                        bail!(
                            "{file} requires {module}, from the mod {owner}: add \"{owner}\" to \
                            its dependencies in mod.json"
                        );
                    }
                }
            }
        }
        let dll = scripts.join("dlls").join("main.dll");
        if dll.is_file() {
            let named = function_paths(&std::fs::read(&dll)?);
            if !named.is_empty() && mod_.detours.is_none() {
                warning!(
                    "dlls/main.dll names {} game function(s) ({}{}); list the ones its code \
                     detours in mod.json's \"detours\" (or [] for none), so that installing \
                     stops when another mod detours one too",
                    named.len(),
                    named.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
                    if named.len() > 3 { ", ..." } else { "" }
                );
            }
            hooks.extend(named);
        }
        for detour in &info.detours {
            hooks.remove(detour);
        }
        info.hooks = hooks.into_iter().collect();

        // New references into /Game/Mods must be cooked assets of this mod, or of a mod it
        // requires,
        // of the class the table field holds
        let mut problems = vec![];
        let mut available = info.assets.clone();
        for entry in &needed {
            available.extend(entry.assets.clone());
        }
        if let Some(result) = &built {
            for (path, class) in &result.references {
                if !path.starts_with("/Game/Mods/") {
                    continue;
                }
                let short = |class: &str| class.rsplit('.').next().unwrap_or(class).to_string();
                match available.get(path) {
                    None => {
                        let package = path.split('.').next().unwrap_or(path);
                        let near: Vec<&str> = available
                            .keys()
                            .filter(|asset| asset.split('.').next() == Some(package))
                            .map(|asset| asset.as_str())
                            .collect();
                        problems.push(format!(
                            "the edits refer to {path}, which neither this mod nor a mod it \
                            requires packages{}",
                            if near.is_empty() {
                                " (add its folder to \"content\" in mod.json, and cook)".to_string()
                            } else {
                                format!(" (that package has {})", near.join(", "))
                            }
                        ));
                    }
                    Some(found) if found != class => problems.push(format!(
                        "the edits use {path} as a {}, but it's a {}",
                        short(class),
                        short(found)
                    )),
                    _ => {}
                }
            }
        }
        // Craft types past Max: the recipes and the machines must agree
        let named: HashSet<u8> = if built.is_some() || !machine_types.is_empty() {
            self.schemas()?
                .enums
                .get("EBeltTDCraftType")
                .map(|values| values.keys().filter_map(|key| key.parse().ok()).collect())
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
        let mut machines: HashSet<u8> = machine_types.values().copied().collect();
        machines.extend(
            needed
                .iter()
                .flat_map(|entry| entry.craft_type_numbers.values().copied()),
        );
        let mut recipe_types: HashSet<u8> = HashSet::new();
        if let Some(result) = &built {
            for (row, recipe) in result.changed.get("DT_EnemyCrafting").into_iter().flatten() {
                if let Some(number) = recipe.get("CraftType").and_then(Value::as_u64) {
                    let number = number as u8;
                    recipe_types.insert(number);
                    if !named.contains(&number) && !machines.contains(&number) {
                        warning!(
                            "recipe {row} has craft type {number}, which no machine \
                            in this mod or the mods it requires runs"
                        );
                    }
                }
            }
        }
        for number in machine_types.values() {
            if !named.contains(number) && !recipe_types.contains(number) {
                warning!(
                    "craft type {number} has no recipes in this mod, so its \
                    machine's list is empty"
                );
            }
        }
        // The rows this mod adds (not those of the mods it requires), and which are buildings
        if let Some(result) = &built {
            let own: HashSet<(String, String)> = own_edits
                .iter()
                .flat_map(|(_, data)| {
                    data.as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(table, _)| !table.starts_with('_'))
                        .flat_map(|(table, rows)| {
                            rows.as_object()
                                .into_iter()
                                .flatten()
                                .map(move |(row, _)| (table.clone(), row.clone()))
                        })
                })
                .collect();
            let mut new_rows: Vec<(String, String)> = result
                .new_rows
                .iter()
                .map(|(t, r, _)| (t.clone(), r.clone()))
                .filter(|key| own.contains(key))
                .collect();
            new_rows.sort();
            for (table, row) in &new_rows {
                if table == "DT_Buildings" {
                    let display = result.changed[table]
                        .get(row)
                        .and_then(|row| row.get("DisplayName"))
                        .and_then(Value::as_str);
                    info.new_buildings.push(
                        display
                            .filter(|text| !text.is_empty())
                            .unwrap_or(row)
                            .to_string(),
                    );
                }
            }
            info.new_rows = new_rows;
        }
        if !problems.is_empty() {
            bail!("build failed:\n   {}", problems.join("\n   "));
        }

        let built_files = files_below(&build_folder);
        if built_files.is_empty() {
            bail!(
                "{}: nothing to build (no edits/*.json, save/, content in mod.json, ue4ss/ or \
                ue4ss-shared/)",
                display_path(&mod_.folder)
            );
        }
        // The mod's own data container and template save are for the zip; installing merges them
        let prefix = PAK_FOLDER.join("/");
        let own_data = |file: &String| {
            [data_container(name), save_container(name)]
                .iter()
                .any(|base| file.starts_with(&format!("{prefix}/{base}.")))
        };
        info.data_files = built_files
            .iter()
            .filter(|file| own_data(file))
            .cloned()
            .collect();
        info.files = built_files
            .iter()
            .filter(|file| !own_data(file) && *file != "build.json")
            .cloned()
            .collect();
        write_json(&build_folder.join("build.json"), &info)?;
        note!(
            "built {name} {} -> {} ({} file(s))",
            mod_.version,
            display_path(&build_folder),
            built_files.len()
        );
        Ok((build_folder, info))
    }

    /// The game's template save with `edits`, as `<folder>/<base>.pak` with its empty container.
    pub(super) fn write_save(&self, edits: &[SaveEdit], folder: &Path, base: &str) -> Result<()> {
        let template = crate::unpack::pak_file(&self.game.pak(), save::TEMPLATE)?;
        let (edited, old) = save::edit_save(&template, edits).context("template save")?;
        for (edit, old) in edits.iter().zip(old) {
            let place = format!(
                "{}{}",
                if edit.occurrence > 0 {
                    format!(" number {}", edit.occurrence + 1)
                } else {
                    String::new()
                },
                edit.after
                    .as_ref()
                    .map(|after| format!(" after {after}"))
                    .unwrap_or_default()
            );
            note!(
                "   template save: {}{place} {old} -> {}",
                edit.property,
                edit.value
            );
        }
        std::fs::create_dir_all(folder)?;
        save::write_template_pak(folder, base, &edited, &self.game)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_requires() {
        let script = "local L = require(\"AlchemistsLibrary\")\nlocal U = require 'UEHelpers'";
        assert_eq!(required_modules(script), ["AlchemistsLibrary", "UEHelpers"]);
    }

    #[test]
    fn hooks_are_found() {
        let script = "RegisterHook(\"/Script/BeltTD.BeltTDInventoryComponent:CheckCanCost\", \
                      function() end)\nRegisterHook(path, function() end)";
        assert_eq!(
            string_arguments(script, "RegisterHook"),
            ["/Script/BeltTD.BeltTDInventoryComponent:CheckCanCost"]
        );
        // C++'s STR("...") is UTF-16, at any offset
        let mut dll = vec![0x90u8; 7];
        for unit in "/Script/BeltTD.BeltTDInventoryHelper:GetInventoryMaximumStack\0".encode_utf16()
        {
            dll.extend(unit.to_le_bytes());
        }
        dll.extend(b"junk /Script/Engine.Actor:ReceiveBeginPlay\0");
        assert_eq!(
            function_paths(&dll),
            [
                "/Script/BeltTD.BeltTDInventoryHelper:GetInventoryMaximumStack",
                "/Script/Engine.Actor:ReceiveBeginPlay"
            ]
        );
    }
}
