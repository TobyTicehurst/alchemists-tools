//! Setting up the stub Unreal project: an empty project, made in the Epic Games Launcher, turned
//! into one whose cooked assets the game can load.
//!
//! 1. Copy the game's C++ module `BeltTD` from the UE4SS header dump, make the dump's cross-module
//!    comments into real `#include`s, and apply the two `Build.cs` fixes.
//! 2. Register the module: in the `.uproject` (with the plugins it needs, for every target) and in
//!    both `Target.cs` files.
//! 3. Copy the game's collision channels and profiles into `Config/DefaultEngine.ini`.
//! 4. Make the cook always include `/Game/Mods` and `/Game/Blueprints` (`Config/DefaultGame.ini`).
//! 5. Build the editor target.
//!
//! Each step changes only what isn't done yet, so running it again is safe. After a game update,
//! `update` repeats steps 1 and 5 when the header dump has changed.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::game::{display_path, join, Game};
use crate::unpack::pak_file;
use crate::update::{copy_folder, fix_includes};

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

/// Plugins the game's module needs, enabled for every target (also the packaged game, so no
/// `TargetAllowList`).
const PLUGINS: &[&str] = &["CommonUI", "EnhancedInput", "OnlineSubsystemUtils"];

const COLLISION_SECTION: &str = "[/Script/Engine.CollisionProfile]";
const PACKAGING_SECTION: &str = "[/Script/UnrealEd.ProjectPackagingSettings]";
const ALWAYS_COOKED: &[&str] = &[
    "+DirectoriesToAlwaysCook=(Path=\"/Game/Mods\")",
    "+DirectoriesToAlwaysCook=(Path=\"/Game/Blueprints\")",
];
/// The game's engine config, inside its `.pak`.
const GAME_ENGINE_INI: &str = "AlchemyFactory/Config/DefaultEngine.ini";

/// The folder of the game's dumped module.
pub fn header_dump(game: &Game) -> PathBuf {
    join(
        &game.root,
        &[
            "AlchemyFactory",
            "Binaries",
            "Win64",
            "ue4ss",
            "UHTHeaderDump",
            "BeltTD",
        ],
    )
}

/// Set up (or finish setting up) the stub project at `project` (its `.uproject`).
pub fn setup(game: &Game, project: &Path, engine: &Path, build: bool) -> Result<()> {
    if !project.is_file() {
        bail!(
            "{} doesn't exist: create a blank C++ project named AlchemyFactory in the Epic Games \
             Launcher (Unreal Engine 5.7.4) first, or pass --project",
            display_path(project)
        );
    }
    if project
        .file_stem()
        .is_none_or(|stem| stem != "AlchemyFactory")
    {
        bail!(
            "{}: the project must be called AlchemyFactory, so that its cooked files land under \
             AlchemyFactory/Content/, the only root the game loads from",
            display_path(project)
        );
    }
    let folder = project.parent().unwrap_or(Path::new("."));

    println!("== the game's module, BeltTD");
    let module = folder.join("Source").join("BeltTD");
    if module.join("BeltTD.Build.cs").is_file() {
        println!(
            "   already in {}: `update` replaces it when the header dump changes",
            display_path(&module)
        );
    } else {
        install_module(
            game,
            project,
            engine,
            &folder.join("Saved").join("BeltTD-before-setup"),
        )?;
    }

    println!("== registering it");
    register_in_project(project)?;
    for target in ["AlchemyFactory.Target.cs", "AlchemyFactoryEditor.Target.cs"] {
        register_in_target(&folder.join("Source").join(target))?;
    }

    println!("== the game's collision settings");
    let game_ini = pak_file(&game.pak(), GAME_ENGINE_INI)?;
    let game_ini = String::from_utf8_lossy(&game_ini).into_owned();
    let section = ini_section(&game_ini, COLLISION_SECTION)
        .with_context(|| format!("no {COLLISION_SECTION} in the game's {GAME_ENGINE_INI}"))?;
    edit_text_file(&folder.join("Config").join("DefaultEngine.ini"), |text| {
        Ok(replace_section(text, COLLISION_SECTION, &section))
    })?;

    println!("== always cooking /Game/Mods and /Game/Blueprints");
    edit_text_file(&folder.join("Config").join("DefaultGame.ini"), |text| {
        Ok(ensure_lines(text, PACKAGING_SECTION, ALWAYS_COOKED))
    })?;

    if build {
        println!("== building (the first build takes a while)");
        build_project(project, engine)?;
        println!(
            "set up {}: open it, and check that C++ Classes > BeltTD lists the game's classes",
            display_path(project)
        );
    } else {
        println!(
            "set up {}; build it before opening it",
            display_path(project)
        );
    }
    Ok(())
}

/// Replace the project's `BeltTD` module with the header dump's (a backup of the old one, if any,
/// goes to `backup`), with real includes and the `Build.cs` fixes.
pub fn install_module(game: &Game, project: &Path, engine: &Path, backup: &Path) -> Result<()> {
    let dump = header_dump(game);
    let public = dump.join("Public");
    let has_headers = std::fs::read_dir(&public)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "h")
        });
    if !has_headers {
        bail!(
            "{} has no header files: start the game with UE4SS in a brand new save and press \
             Ctrl + Numpad 9 once it has loaded (a dump with only .cpp files is incomplete)",
            display_path(&public)
        );
    }
    let source = join(
        project.parent().unwrap_or(Path::new(".")),
        &["Source", "BeltTD"],
    );
    if source.is_dir() {
        let _ = std::fs::remove_dir_all(backup);
        copy_folder(&source, backup)?;
        println!("   backed up the module to {}", display_path(backup));
        std::fs::remove_dir_all(&source)?;
    }
    copy_folder(&dump, &source)?;
    println!("   copied the header dump to {}", display_path(&source));
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
    Ok(())
}

/// Build the project's editor target.
pub fn build_project(project: &Path, engine: &Path) -> Result<()> {
    let build = join(engine, &["Engine", "Build", "BatchFiles", "Build.bat"]);
    if !build.is_file() {
        bail!(
            "{} not found: install Unreal Engine 5.7 from the Epic Games Launcher, or pass \
             --engine",
            display_path(&build)
        );
    }
    // -MaxParallelActions=4 avoids "The paging file is too small" on a full build
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

/// Add the module, and the plugins it needs for every target, to the `.uproject`.
fn register_in_project(project: &Path) -> Result<()> {
    let text = std::fs::read_to_string(project)?;
    let mut uproject: Value = serde_json::from_str(&text)
        .with_context(|| format!("reading {}", display_path(project)))?;
    let mut changes = vec![];

    let modules = uproject
        .as_object_mut()
        .context("the .uproject isn't an object")?
        .entry("Modules")
        .or_insert_with(|| json!([]));
    let modules = modules.as_array_mut().context("Modules isn't a list")?;
    if !modules.iter().any(|module| module["Name"] == "BeltTD") {
        modules.push(json!({"Name": "BeltTD", "Type": "Runtime", "LoadingPhase": "Default"}));
        changes.push("added the module BeltTD".to_string());
    }

    let plugins = uproject
        .as_object_mut()
        .unwrap()
        .entry("Plugins")
        .or_insert_with(|| json!([]));
    let plugins = plugins.as_array_mut().context("Plugins isn't a list")?;
    for name in PLUGINS {
        match plugins.iter_mut().find(|plugin| plugin["Name"] == *name) {
            Some(plugin) => {
                let plugin = plugin.as_object_mut().context("a plugin isn't an object")?;
                if plugin.get("Enabled") != Some(&json!(true)) {
                    plugin.insert("Enabled".into(), json!(true));
                    changes.push(format!("enabled the plugin {name}"));
                }
                if plugin.remove("TargetAllowList").is_some() {
                    changes.push(format!(
                        "enabled {name} for every target, not only the editor"
                    ));
                }
            }
            None => {
                plugins.push(json!({"Name": name, "Enabled": true}));
                changes.push(format!("enabled the plugin {name}"));
            }
        }
    }

    if changes.is_empty() {
        println!("   {}: already done", display_path(project));
        return Ok(());
    }
    // the Launcher's own layout: tabs
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(&uproject, &mut serializer)?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let out = String::from_utf8(out)?.replace('\n', newline);
    std::fs::write(project, out)?;
    for change in changes {
        println!("   {}: {change}", display_path(project));
    }
    Ok(())
}

/// Add `ExtraModuleNames.Add("BeltTD");` to a `Target.cs`, after the project's own module.
fn register_in_target(target: &Path) -> Result<()> {
    edit_text_file(target, |text| {
        if text.contains("ExtraModuleNames.Add(\"BeltTD\");") {
            return Ok(text.to_string());
        }
        let anchor = "ExtraModuleNames.Add(\"AlchemyFactory\");";
        let Some(line) = text.lines().find(|line| line.trim() == anchor) else {
            bail!(
                "{}: no line {anchor}, so add ExtraModuleNames.Add(\"BeltTD\"); to it by hand",
                display_path(target)
            );
        };
        let indent = &line[..line.len() - line.trim_start().len()];
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        Ok(text.replacen(
            line,
            &format!("{line}{newline}{indent}ExtraModuleNames.Add(\"BeltTD\");"),
            1,
        ))
    })
}

/// Rewrite a text file through `change`, reporting whether anything changed. Line endings are
/// kept.
fn edit_text_file(path: &Path, change: impl FnOnce(&str) -> Result<String>) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let crlf = text.contains("\r\n");
    let plain = text.replace("\r\n", "\n");
    let changed = change(&plain)?;
    if changed == plain {
        println!("   {}: already done", display_path(path));
        return Ok(());
    }
    let out = if crlf {
        changed.replace('\n', "\r\n")
    } else {
        changed
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", display_path(path)))?;
    println!("   {}: updated", display_path(path));
    Ok(())
}

/// The lines of an ini section, its header included, without trailing blank lines.
fn ini_section(text: &str, header: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n");
    let mut lines = text.lines().skip_while(|line| line.trim() != header);
    let first = lines.next()?;
    let mut section = vec![first];
    section.extend(lines.take_while(|line| !line.starts_with('[')));
    while section.last().is_some_and(|line| line.trim().is_empty()) {
        section.pop();
    }
    Some(section.join("\n"))
}

/// `text` with the section `header` replaced by `section`, or with `section` added at the end.
fn replace_section(text: &str, header: &str, section: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == header) else {
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(section);
        out.push('\n');
        return out;
    };
    let mut end = start + 1;
    while end < lines.len() && !lines[end].starts_with('[') {
        end += 1;
    }
    // keep the blank line that separated it from the next section
    let mut kept_blank = end;
    while kept_blank > start + 1 && lines[kept_blank - 1].trim().is_empty() {
        kept_blank -= 1;
    }
    let mut out: Vec<&str> = lines[..start].to_vec();
    out.extend(section.lines());
    out.extend(&lines[kept_blank..]);
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// `text` with every one of `wanted` in the section `header` (made if missing).
fn ensure_lines(text: &str, header: &str, wanted: &[&str]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == header) else {
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(header);
        for line in wanted {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        return out;
    };
    let mut end = start + 1;
    while end < lines.len() && !lines[end].starts_with('[') {
        end += 1;
    }
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|line| {
            !lines[start + 1..end]
                .iter()
                .any(|existing| existing.trim() == **line)
        })
        .copied()
        .collect();
    if missing.is_empty() {
        return text.to_string();
    }
    let mut out: Vec<&str> = lines[..=start].to_vec();
    out.extend(missing);
    out.extend(&lines[start + 1..]);
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_sections() {
        let game = "[A]\nx=1\n\n[/Script/Engine.CollisionProfile]\n\
                    +Profiles=(Name=\"Furniture\")\n\n[B]\ny=2\n";
        let section = ini_section(game, COLLISION_SECTION).unwrap();
        assert_eq!(
            section,
            "[/Script/Engine.CollisionProfile]\n+Profiles=(Name=\"Furniture\")"
        );
        // added once at the end, then replaced in place
        let project = "[A]\nx=1\n";
        let added = replace_section(project, COLLISION_SECTION, &section);
        assert_eq!(added, format!("[A]\nx=1\n\n{section}\n"));
        assert_eq!(replace_section(&added, COLLISION_SECTION, &section), added);
        let old = format!("{}\n-Profiles=old\n\n[B]\ny=2\n", COLLISION_SECTION);
        assert_eq!(
            replace_section(&old, COLLISION_SECTION, &section),
            format!("{section}\n\n[B]\ny=2\n")
        );
        // lines are added only where missing
        let cooked = ensure_lines("[C]\n", PACKAGING_SECTION, ALWAYS_COOKED);
        assert_eq!(
            cooked,
            format!(
                "[C]\n\n{PACKAGING_SECTION}\n{}\n{}\n",
                ALWAYS_COOKED[0], ALWAYS_COOKED[1]
            )
        );
        assert_eq!(
            ensure_lines(&cooked, PACKAGING_SECTION, ALWAYS_COOKED),
            cooked
        );
        let half = format!("{PACKAGING_SECTION}\n{}\n", ALWAYS_COOKED[1]);
        assert_eq!(
            ensure_lines(&half, PACKAGING_SECTION, ALWAYS_COOKED),
            format!(
                "{PACKAGING_SECTION}\n{}\n{}\n",
                ALWAYS_COOKED[0], ALWAYS_COOKED[1]
            )
        );
    }
}
