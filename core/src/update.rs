//! After a game update: refresh everything the tools take from the game, and rebuild the mods.
//!
//! 1. Check that UE4SS's `.usmap` and header dump are newer than the game's executable. If not,
//!    stop: start the game with UE4SS and press Ctrl + Numpad 6 (mappings) and Ctrl + Numpad 9
//!    (headers) first.
//! 2. Re-extract the game's tables and report which rows changed.
//! 3. If the header dump changed, rebuild the stub project's `BeltTD` module from it: copy it,
//!    turn the dump's cross-module comments into real `#include`s, apply the two `Build.cs`
//!    fixes, build.
//! 4. Decode the game Blueprints that mods copy (the ScriptClass of each `_copy` source in
//!    DT_Buildings) and compare them with the snapshot from the last update.
//! 5. Rebuild every mod, and re-merge the installed mods' table edits against the new tables.
//!
//! What the last run saw is kept in `<build>/.update/`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::{json, Value};

use crate::game::{display_path, join, Game};
use crate::mods::{game_build, Workspace};

/// `Build.cs` fixes the dumped module needs (UE 5.6 renamed the setting; the session-search code
/// links against OnlineSubsystem).
const BUILD_CS_FIXES: &[(&str, &str)] = &[
    (
        "        ShadowVariableWarningLevel = ",
        "        CppCompileWarningSettings.ShadowVariableWarningLevel = ",
    ),
    (
        "            \"OnlineSubsystemUtils\",",
        "            \"OnlineSubsystem\",\n            \"OnlineSubsystemUtils\",",
    ),
];

/// CoreUObject's script structs (FVector, FColor, ...) are declared for UHT only in
/// NoExportTypes.h, which mustn't be included; their real declarations come with CoreMinimal.h.
const CORE_UOBJECT_HEADERS: &[(&str, &str)] = &[
    ("Object", "UObject/Object.h"),
    ("Interface", "UObject/Interface.h"),
    ("EInterpCurveMode", "Math/InterpCurvePoint.h"),
];

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn files_below(folder: &Path, extension: Option<&str>) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut todo = vec![folder.to_path_buf()];
    while let Some(current) = todo.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if extension
                .is_none_or(|wanted| path.extension().is_some_and(|found| found == wanted))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn ue4ss(game: &Game) -> PathBuf {
    join(
        &game.root,
        &["AlchemyFactory", "Binaries", "Win64", "ue4ss"],
    )
}

/// Whether the `.usmap` and header dump are newer than the game; what's stale if not.
pub fn stale_dumps(game: &Game, usmap: &Path) -> Vec<String> {
    let exe = join(
        &game.root,
        &[
            "AlchemyFactory",
            "Binaries",
            "Win64",
            "AlchemyFactory-Win64-Shipping.exe",
        ],
    );
    let Some(game_time) = modified(&exe) else {
        return vec![];
    };
    let mut stale = vec![];
    if modified(usmap).is_none_or(|time| time < game_time) {
        stale.push("the .usmap (Ctrl + Numpad 6)".to_string());
    }
    let headers = files_below(
        &join(&ue4ss(game), &["UHTHeaderDump", "BeltTD", "Public"]),
        Some("h"),
    );
    match headers.iter().filter_map(|path| modified(path)).max() {
        None => stale.push(
            "the header dump (Ctrl + Numpad 9; check UHTHeaderDump/BeltTD/Public has .h files)"
                .to_string(),
        ),
        Some(time) if time < game_time => {
            stale.push("the header dump (Ctrl + Numpad 9)".to_string())
        }
        _ => {}
    }
    stale
}

/// Every extracted table's rows: name -> rows.
fn extracted_tables(data: &Path) -> BTreeMap<String, serde_json::Map<String, Value>> {
    let mut out = BTreeMap::new();
    for path in files_below(data, Some("json")) {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("DT_") {
            continue;
        }
        let rows = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|table| table.get("rows").and_then(Value::as_object).cloned());
        if let Some(rows) = rows {
            out.insert(name, rows);
        }
    }
    out
}

fn report_table_changes(
    before: &BTreeMap<String, serde_json::Map<String, Value>>,
    after: &BTreeMap<String, serde_json::Map<String, Value>>,
) {
    let mut changed = false;
    let empty = serde_json::Map::new();
    let names: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for table in names {
        let (old, new) = (
            before.get(table).unwrap_or(&empty),
            after.get(table).unwrap_or(&empty),
        );
        let added: Vec<&String> = new.keys().filter(|row| !old.contains_key(*row)).collect();
        let removed: Vec<&String> = old.keys().filter(|row| !new.contains_key(*row)).collect();
        let edited: Vec<&String> = new
            .keys()
            .filter(|row| old.get(*row).is_some_and(|value| value != &new[*row]))
            .collect();
        let mut parts = vec![];
        for (what, rows) in [("new", &added), ("removed", &removed), ("changed", &edited)] {
            if !rows.is_empty() {
                let shown: Vec<&str> = rows.iter().take(6).map(|row| row.as_str()).collect();
                parts.push(format!(
                    "{} {what} ({}{})",
                    rows.len(),
                    shown.join(", "),
                    if rows.len() > 6 { ", ..." } else { "" }
                ));
            }
        }
        if !parts.is_empty() {
            changed = true;
            println!("   {table}: {}", parts.join("; "));
        }
    }
    if !changed {
        println!("   no table changed");
    }
}

fn same_tree(a: &Path, b: &Path) -> bool {
    let (left, right) = (files_below(a, None), files_below(b, None));
    let relative = |files: &[PathBuf], root: &Path| -> Vec<PathBuf> {
        files
            .iter()
            .filter_map(|file| file.strip_prefix(root).ok().map(Path::to_path_buf))
            .collect()
    };
    let (left_names, right_names) = (relative(&left, a), relative(&right, b));
    left_names == right_names
        && left_names
            .iter()
            .all(|name| std::fs::read(a.join(name)).ok() == std::fs::read(b.join(name)).ok())
}

fn copy_folder(from: &Path, to: &Path) -> Result<()> {
    for file in files_below(from, None) {
        let target = to.join(file.strip_prefix(from)?);
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::copy(&file, &target)?;
    }
    Ok(())
}

/// Module name -> its folder (where its .Build.cs is), for the engine and its plugins.
fn module_folders(engine: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    for top in ["Source", "Plugins"] {
        let mut todo = vec![engine.join(top)];
        while let Some(folder) = todo.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    if !["Intermediate", "Binaries", "Content", "ThirdParty"]
                        .contains(&name.as_str())
                    {
                        todo.push(path);
                    }
                } else if let Some(module) = name.strip_suffix(".Build.cs") {
                    out.entry(module.to_string()).or_insert(folder.clone());
                }
            }
        }
    }
    out
}

/// The header (as an include path) that declares `object` in a module, or None.
fn find_header(module: &Path, object: &str) -> Option<String> {
    let name = regex::escape(object);
    let patterns: Vec<Regex> = if let Some(delegate) = object.strip_suffix("__DelegateSignature") {
        vec![Regex::new(&format!(
            r"DECLARE_\w*DELEGATE\w*\(\s*F{}\s*,",
            regex::escape(delegate)
        ))
        .ok()?]
    } else {
        vec![
            Regex::new(&format!(
                concat!(
                    r"(?m)^\s*(?:class|struct)\s+(?:\w+_API\s+)?[UAFI]{}\b",
                    r"\s*(?:final\s*)?(?::|\{{|$)"
                ),
                name
            ))
            .ok()?,
            Regex::new(&format!(r"(?m)^\s*enum\s+(?:class\s+)?{name}\b")).ok()?,
            Regex::new(&format!(r"(?m)^\s*UENUM.*\n\s*namespace\s+{name}\b")).ok()?,
        ]
    };
    let stem = object.trim_end_matches("__DelegateSignature");
    let mut candidates: Vec<(bool, bool, usize, String)> = vec![];
    for file in files_below(module, Some("h")) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if patterns.iter().any(|pattern| pattern.is_match(&text)) {
            let relative = file
                .strip_prefix(module)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let public = relative.starts_with("Public/")
                || relative.starts_with("Classes/")
                || relative.contains("/Public/")
                || relative.contains("/Classes/");
            let named = file.file_stem().is_some_and(|found| found == stem);
            candidates.push((!public, !named, relative.len(), relative));
        }
    }
    let (_, _, _, best) = candidates.into_iter().min()?;
    Some(
        ["Public/", "Classes/"]
            .iter()
            .find_map(|prefix| best.strip_prefix(prefix))
            .unwrap_or(&best)
            .to_string(),
    )
}

/// UE4SS's header dump leaves `//CROSS-MODULE INCLUDE V2: -ModuleName=X -ObjectName=Y ...`
/// comments instead of includes from other modules; put the real `#include` in front of each.
pub fn fix_includes(source: &Path, engine: &Path) -> Result<usize> {
    let comment = Regex::new(
        r"^//CROSS-MODULE INCLUDE V2: -ModuleName=(\w+) -ObjectName=(\w+) -FallbackName=\w+\s*$",
    )?;
    let modules = module_folders(engine);
    let mut resolved: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut changed_files = 0;
    for file in files_below(source, None) {
        if !file
            .extension()
            .is_some_and(|extension| extension == "h" || extension == "cpp")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file)?;
        let mut changed = false;
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| {
                let Some(found) = comment.captures(line) else {
                    return line.to_string();
                };
                let key = (found[1].to_string(), found[2].to_string());
                let header = resolved
                    .entry(key.clone())
                    .or_insert_with(|| {
                        if key.0 == "CoreUObject" {
                            Some(
                                CORE_UOBJECT_HEADERS
                                    .iter()
                                    .find(|(name, _)| *name == key.1)
                                    .map(|(_, h)| h.to_string())
                                    .unwrap_or_default(),
                            )
                        } else {
                            modules
                                .get(&key.0)
                                .and_then(|folder| find_header(folder, &key.1))
                        }
                    })
                    .clone();
                match header {
                    Some(header) if !header.is_empty() => {
                        changed = true;
                        format!("#include \"{header}\" {line}")
                    }
                    _ => line.to_string(),
                }
            })
            .collect();
        if changed {
            std::fs::write(&file, lines.join("\n"))?;
            changed_files += 1;
        }
    }
    let unresolved: Vec<String> = resolved
        .iter()
        .filter(|(_, header)| header.is_none())
        .map(|((m, o), _)| format!("{m}.{o}"))
        .collect();
    if !unresolved.is_empty() {
        println!("   unresolved includes: {}", unresolved.join(", "));
    }
    Ok(changed_files)
}

fn rebuild_stub(game: &Game, project: &Path, engine: &Path, state: &Path) -> Result<()> {
    let dump = join(&ue4ss(game), &["UHTHeaderDump", "BeltTD"]);
    let source = join(
        project.parent().unwrap_or(Path::new(".")),
        &["Source", "BeltTD"],
    );
    let backup = state.join("BeltTD-before-update");
    let _ = std::fs::remove_dir_all(&backup);
    copy_folder(&source, &backup)?;
    println!("   backed up the module to {}", display_path(&backup));
    std::fs::remove_dir_all(&source)?;
    copy_folder(&dump, &source)?;
    println!(
        "   fixed the includes of {} file(s)",
        fix_includes(&source, &engine.join("Engine"))?
    );
    let build_cs = source.join("BeltTD.Build.cs");
    let mut text = std::fs::read_to_string(&build_cs)?;
    for (old, new) in BUILD_CS_FIXES {
        if text.contains(new) {
            continue;
        }
        if !text.contains(old) {
            bail!(
                "{}: can't apply the fix {:?}; the dumped file has changed, so edit it by hand and \
                build",
                display_path(&build_cs),
                new.trim()
            );
        }
        text = text.replacen(old, new, 1);
    }
    std::fs::write(&build_cs, text)?;
    println!("   applied the Build.cs fixes");
    let build = join(engine, &["Engine", "Build", "BatchFiles", "Build.bat"]);
    let status = std::process::Command::new("cmd")
        .args(["/c"])
        .arg(&build)
        .args(["AlchemyFactoryEditor", "Win64", "Development"])
        .arg(format!("-Project={}", project.display()))
        .args(["-WaitMutex", "-NoHotReloadFromIDE", "-MaxParallelActions=4"])
        .status()
        .context("running Build.bat")?;
    if !status.success() {
        bail!("building the stub project failed");
    }
    Ok(())
}

/// The game Blueprints that mods copy: the ScriptClass of each building a mod's DT_Buildings edit
/// copies, as package path -> the mods that copy it.
fn copied_blueprints(workspace: &Workspace, data: &Path) -> BTreeMap<String, Vec<String>> {
    let buildings = extracted_tables(data)
        .remove("DT_Buildings")
        .unwrap_or_default();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in std::fs::read_dir(&workspace.mods)
        .into_iter()
        .flatten()
        .flatten()
    {
        let mod_name = entry.file_name().to_string_lossy().into_owned();
        for file in files_below(&entry.path().join("edits"), Some("json")) {
            let Some(rows) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            for row in rows
                .get("DT_Buildings")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(_, row)| row)
            {
                let script = row
                    .get("_copy")
                    .and_then(Value::as_str)
                    .and_then(|source| buildings.get(source))
                    .and_then(|source| source.get("ScriptClass"))
                    .and_then(Value::as_str);
                if let Some(package) = script.and_then(|path| path.split('.').next()) {
                    let users = out.entry(package.to_string()).or_default();
                    if !users.contains(&mod_name) {
                        users.push(mod_name.clone());
                    }
                }
            }
        }
    }
    out
}

fn compare_blueprints(workspace: &Workspace, data: &Path, state: &Path) -> Result<()> {
    let packages = copied_blueprints(workspace, data);
    if packages.is_empty() {
        println!("   no mod copies a game building");
        return Ok(());
    }
    let schemas = crate::usmap::read_usmap(&workspace.usmap)?;
    let wanted: Vec<String> = packages
        .keys()
        .map(|package| {
            format!(
                "AlchemyFactory/Content/{}.uasset",
                &package["/Game/".len()..]
            )
        })
        .collect();
    let found = crate::unpack::legacy_assets(
        &workspace.game.paks(),
        |path| wanted.contains(&path.to_string()),
        |_| true,
    )?;
    let snapshots = state.join("blueprint-snapshots");
    std::fs::create_dir_all(&snapshots)?;
    for (package, users) in &packages {
        let path = format!("AlchemyFactory/Content/{}", &package["/Game/".len()..]);
        let Ok((uasset, uexp)) = found.asset(&path) else {
            println!(
                "   {package} (copied by {}): NOT FOUND in the game; those mods need a new base",
                users.join(", ")
            );
            continue;
        };
        let components: serde_json::Map<String, Value> =
            crate::blueprint::components(uasset, uexp, &schemas)?
                .into_iter()
                .map(|c| {
                    (
                        c.name,
                        json!({"class": c.class, "parent": c.parent, "values": c.values}),
                    )
                })
                .collect();
        let snapshot = snapshots.join(format!(
            "{}.json",
            package.trim_start_matches('/').replace('/', "_")
        ));
        match std::fs::read_to_string(&snapshot)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        {
            Some(old) if old == Value::Object(components.clone()) => {
                println!("   {package}: unchanged")
            }
            Some(old) => {
                println!(
                    "   {package} (copied by {}) CHANGED; compare the mods' Blueprints:",
                    users.join(", ")
                );
                let old = old.as_object().cloned().unwrap_or_default();
                let names: std::collections::BTreeSet<&String> =
                    old.keys().chain(components.keys()).collect();
                for name in names {
                    match (old.get(name), components.get(name)) {
                        (None, _) => println!("      component {name}: new"),
                        (_, None) => println!("      component {name}: removed"),
                        (Some(a), Some(b)) if a != b => println!("      component {name}: changed"),
                        _ => {}
                    }
                }
            }
            None => println!("   {package}: snapshot taken (compared from the next update on)"),
        }
        std::fs::write(
            &snapshot,
            serde_json::to_string_pretty(&Value::Object(components))?,
        )?;
    }
    Ok(())
}

/// What `update` was asked to do.
pub struct Options {
    pub data: PathBuf,
    pub engine: PathBuf,
    pub skip_stub: bool,
    pub force: bool,
    pub allow_old_dumps: bool,
}

pub fn update(workspace: &Workspace, options: &Options) -> Result<()> {
    let state = workspace.build.join(".update");
    std::fs::create_dir_all(&state)?;
    let state_file = state.join("game-update.json");
    let last: Value = std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(json!({}));
    let build = game_build(&workspace.game);
    println!(
        "game build {}; last update run: {}",
        build.as_deref().unwrap_or("(unknown)"),
        last.get("game-build")
            .and_then(Value::as_str)
            .unwrap_or("never")
    );
    let updated =
        options.force || build.as_deref() != last.get("game-build").and_then(Value::as_str);

    println!("\n== 1. UE4SS dumps");
    let stale = stale_dumps(&workspace.game, &workspace.usmap);
    let dumps_current = stale.is_empty();
    if dumps_current {
        println!("   .usmap and header dump are newer than the game");
    } else {
        let message = format!(
            "the game is newer than {}.\nStart the game with UE4SS, load a world, press those \
            keys, close the game, and \
             run this again.",
            stale.join(" and ")
        );
        if !options.allow_old_dumps {
            bail!("{message}\n(--allow-old-dumps goes on anyway: the tables don't need the dumps)");
        }
        println!("   warning: {}", message.replace('\n', "\n   "));
    }

    println!("\n== 2. game tables");
    if updated {
        let before = extracted_tables(&options.data);
        crate::extract::extract(&workspace.game, &workspace.usmap, &options.data)?;
        report_table_changes(&before, &extracted_tables(&options.data));
    } else {
        println!("   same game build: not re-extracted (--force to redo)");
    }

    println!("\n== 3. stub project");
    let dump = join(&ue4ss(&workspace.game), &["UHTHeaderDump", "BeltTD"]);
    let last_dump = state.join("header-dump").join("BeltTD");
    if !last_dump.is_dir() {
        copy_folder(&dump, &last_dump)?;
        println!(
            "   recorded the current header dump; the next update rebuilds the stub if it changes"
        );
    } else if same_tree(&dump, &last_dump) && !options.force {
        println!("   header dump unchanged: no rebuild needed");
    } else if !dumps_current {
        println!("   the header dump is older than the game: not rebuilding from it");
    } else if options.skip_stub {
        println!("   the header dump changed, but --skip-stub: rebuild the stub before cooking");
    } else {
        rebuild_stub(&workspace.game, &workspace.project, &options.engine, &state)?;
        let _ = std::fs::remove_dir_all(&last_dump);
        copy_folder(&dump, &last_dump)?;
        println!("   rebuilt; re-cook the mods' content in the editor before packaging it");
    }

    println!("\n== 4. game Blueprints that mods copy");
    compare_blueprints(workspace, &options.data, &state)?;

    println!("\n== 5. mods");
    let mut failed = vec![];
    let mut folders: Vec<PathBuf> = std::fs::read_dir(&workspace.mods)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("mod.json").exists())
        .collect();
    folders.sort();
    for folder in folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        match crate::mods::Mod::load(&folder).and_then(|found| workspace.build_mod(&found)) {
            Ok(_) => {}
            Err(error) => {
                println!("   {name}: FAILED: {error:#}");
                failed.push(name);
            }
        }
    }
    workspace.remerge()?;

    if dumps_current {
        std::fs::write(
            &state_file,
            serde_json::to_string_pretty(&json!({"game-build": build}))?,
        )?;
    } else {
        println!("\nnot recorded as done: run it again once the dumps are new");
    }
    println!(
        "\n{}",
        if failed.is_empty() {
            "done".to_string()
        } else {
            format!(
                "done, but these mods failed to build: {}",
                failed.join(", ")
            )
        }
    );
    Ok(())
}
