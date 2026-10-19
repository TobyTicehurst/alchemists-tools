//! Tests that need the installed game (and its `.usmap`). Game files are never committed, so these
//! are ignored by default: `cargo test -- --ignored` runs them, and each one skips itself when the
//! game can't be found.

use alchemists_core::extract;
use alchemists_core::game::Game;
use alchemists_core::schema::Schemas;
use alchemists_core::unpack::Assets;
use alchemists_core::usmap::read_usmap;
use alchemists_core::write::Table;

fn game() -> Option<(Game, Schemas, Assets)> {
    let Some(game) = Game::find() else {
        eprintln!("game not found: skipping");
        return None;
    };
    let schemas = read_usmap(&game.find_usmap().expect("no .usmap")).unwrap();
    let assets = extract::content_assets(&game).unwrap();
    Some((game, schemas, assets))
}

/// Each plain DataTable in the game, as (path, .uasset, .uexp).
fn tables<'a>(assets: &'a Assets, schemas: &Schemas) -> Vec<(&'a str, &'a [u8], &'a [u8])> {
    let mut out = vec![];
    for path in assets.of_class(&["DataTable"]) {
        let (uasset, uexp) = assets.asset(path).unwrap();
        // Two Ultra Dynamic Sky editor tables use structs the .usmap doesn't have
        if Table::open(uasset, uexp, schemas).is_ok() {
            out.push((path, uasset, uexp));
        }
    }
    assert!(out.len() > 30, "only {} tables", out.len());
    out
}

/// Saving a table without changes gives back its exact bytes.
#[test]
#[ignore]
fn unchanged_tables_are_identical() {
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    for (path, uasset, uexp) in tables(&assets, &schemas) {
        let table = Table::open(uasset, uexp, &schemas).unwrap();
        let (written_uasset, written_uexp) = table.save().unwrap();
        assert!(written_uasset == uasset, "{path}: .uasset differs");
        assert!(written_uexp == uexp, "{path}: .uexp differs");
    }
}

/// Re-encoding every row from its decoded values gives back the game's own bytes: every encoder,
/// for every value in the game's tables, matches the cooker.
#[test]
#[ignore]
fn reencoded_tables_are_identical() {
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    let mut differing = vec![];
    for (path, uasset, uexp) in tables(&assets, &schemas) {
        let table = Table::open(uasset, uexp, &schemas).unwrap();
        match table.reencode_all() {
            Ok(written) if written == uexp => {}
            Ok(written) => {
                let first = written.iter().zip(uexp).position(|(a, b)| a != b);
                differing.push(format!(
                    "{path}: {} bytes, {} expected, first difference at {first:?}",
                    written.len(),
                    uexp.len()
                ));
            }
            Err(error) => differing.push(format!("{path}: {error:#}")),
        }
    }
    assert!(
        differing.is_empty(),
        "{} differ:\n{}",
        differing.len(),
        differing.join("\n")
    );
}

/// MyMachine's edits (`mods/MyMachine/edits/my-machine.json` in the mod project), written to
/// `<target tmp>/my-machine/` with the originals, to compare with the Python writer's output.
#[test]
#[ignore]
fn write_my_machine() {
    use serde_json::json;
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("my-machine");
    std::fs::create_dir_all(&out).unwrap();
    let path = "AlchemyFactory/Content/DataTables/";
    for (name, row_name, like, changes) in [
        (
            "DT_Buildings",
            "MyMachine",
            "Processor",
            json!({
                "ID": 220,
                "ScriptClass": "/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C",
                "AlterBuildOption": "None",
                "CostList": [{"IngredientName": "WoodBoard", "Count": 1}],
                "DisplayName": "My Machine",
                "DisplayDesc": "A test machine: an Assembler in a Processor's body. One conveyor \
                                belt \
                                input and one conveyor belt output."
            }),
        ),
        (
            "DT_SkillPoints",
            "MyMachine",
            "Processor",
            json!({
                "SlotPosition": {"X": -2, "Y": 4},
                "Predecessors": ["Processor"],
                "UnlockItem": {"ConfigName": "MyMachine", "ConfigType": "ConstructOptions"}
            }),
        ),
    ] {
        let (uasset, uexp) = assets.asset(&format!("{path}{name}")).unwrap();
        std::fs::write(out.join(format!("{name}.original.uasset")), uasset).unwrap();
        std::fs::write(out.join(format!("{name}.original.uexp")), uexp).unwrap();
        let mut table = Table::open(uasset, uexp, &schemas).unwrap();
        let mut row = table.rows[like].clone();
        for (field, value) in changes.as_object().unwrap() {
            if let Some(object) = value.as_object().filter(|_| row[field].is_object()) {
                for (key, inner) in object {
                    row[field][key] = inner.clone();
                }
            } else {
                row[field] = value.clone();
            }
        }
        table.add_row(row_name, row, like).unwrap();
        let (new_uasset, new_uexp) = table.save().unwrap();
        std::fs::write(out.join(format!("{name}.uasset")), new_uasset).unwrap();
        std::fs::write(out.join(format!("{name}.uexp")), new_uexp).unwrap();
    }
    println!("written to {}", out.display());
}

/// New references take their field's class, or the one given with them; a class the field doesn't
/// hold is refused.
#[test]
#[ignore]
fn new_reference_classes() {
    use std::io::Cursor;

    use alchemists_core::unpack::package_version;
    use retoc::legacy_asset::FLegacyPackageHeader;
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    let (uasset, uexp) = assets
        .asset("AlchemyFactory/Content/DataTables/DT_Buildings")
        .unwrap();
    let mut table = Table::open(uasset, uexp, &schemas).unwrap();
    let mut row = table.rows["Processor"].clone();
    row["DisplayIcon"] = "/Game/Mods/MyMachine/T_UI_MyMachine.T_UI_MyMachine".into();
    row["InstanceMesh"] = "/Script/Engine.StaticMesh'/Game/Mods/MyMachine/SM_Body.SM_Body'".into();
    table
        .add_row("MyMachine", row.clone(), "Processor")
        .unwrap();
    let (written, _) = table.save().unwrap();
    let header =
        FLegacyPackageHeader::deserialize(&mut Cursor::new(&written), Some(package_version()))
            .unwrap();
    let name = |minimal| header.name_map.get(minimal).unwrap().into_owned();
    let classes: Vec<(String, String)> = header
        .imports
        .iter()
        .map(|import| (name(import.object_name), name(import.class_name)))
        .collect();
    for (object, class) in [
        ("T_UI_MyMachine", "Texture2D"),
        ("SM_Body", "StaticMesh"),
        ("/Game/Mods/MyMachine/T_UI_MyMachine", "Package"),
    ] {
        assert!(
            classes.contains(&(object.to_string(), class.to_string())),
            "no {class} import {object}"
        );
    }
    row["DisplayIcon"] = "/Script/Engine.StaticMesh'/Game/Mods/X/SM_A.SM_A'".into();
    table.add_row("MyMachine", row, "Processor").unwrap();
    let error = table.save().unwrap_err().to_string();
    assert!(error.contains("the field holds Texture2D"), "{error}");
}

/// Tables packed into a container come back the same when converted back to legacy format.
#[test]
#[ignore]
fn container_round_trip() {
    use alchemists_core::package::{script_objects, write_container, LegacyAsset};
    use alchemists_core::unpack::legacy_assets;
    use retoc::version::EngineVersion;
    let Some((game, schemas, assets)) = game() else {
        return;
    };
    let mut written = vec![];
    for name in ["DT_Buildings", "DT_SkillPoints"] {
        let path = format!("AlchemyFactory/Content/DataTables/{name}");
        let (uasset, uexp) = assets.asset(&path).unwrap();
        let mut table = Table::open(uasset, uexp, &schemas).unwrap();
        let mut row = table.rows["Processor"].clone();
        row["ID"] = 299.into(); // a change, so the row is re-encoded
        table.add_row("RoundTrip", row, "Processor").unwrap();
        let (uasset, uexp) = table.save().unwrap();
        written.push(LegacyAsset {
            path,
            uasset,
            uexp,
            ubulk: None,
        });
    }
    let folder = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("container");
    let _ = std::fs::remove_dir_all(&folder);
    let utoc = folder.join("zzz_RoundTrip_P.utoc");
    write_container(
        &utoc,
        &written,
        EngineVersion::UE5_6,
        script_objects(&game).unwrap(),
    )
    .unwrap();
    // With the game's own containers (hard links), every import resolves to a name
    for file in std::fs::read_dir(game.paks()).unwrap() {
        let file = file.unwrap().path();
        if file.is_file() {
            std::fs::hard_link(&file, folder.join(file.file_name().unwrap())).unwrap();
        }
    }
    let wanted: Vec<String> = written
        .iter()
        .map(|asset| format!("{}.uasset", asset.path))
        .collect();
    let back = legacy_assets(&folder, |path| wanted.iter().any(|w| w == path), |_| true).unwrap();
    for asset in &written {
        let (uasset, uexp) = back.asset(&asset.path).unwrap();
        assert!(
            uexp == asset.uexp.as_slice(),
            "{}: .uexp differs",
            asset.path
        );
        assert!(
            uasset == asset.uasset.as_slice(),
            "{}: .uasset differs",
            asset.path
        );
    }
}

fn edit_game<'a>(schemas: &'a Schemas, assets: &'a Assets) -> alchemists_core::edits::Game<'a> {
    alchemists_core::edits::Game {
        assets,
        schemas,
        localizer: None,
        packages: None,
    }
}

/// An edit file with a new building, its research, a recipe change and a new recipe.
#[test]
#[ignore]
fn edits_build() {
    use alchemists_core::edits::{build, Source};
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    let contents = serde_json::json!({
        "DT_Buildings": {"MyMachine": {
            "_copy": "Processor", "AlterBuildOption": "None",
            "ScriptClass": "/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C",
            "CostList": [{"IngredientName": "WoodBoard", "Count": 1}]}},
        "DT_SkillPoints": {"MyMachine": {
            "_copy": "Processor", "SlotPosition": {"X": -2, "Y": 4}, "Predecessors": ["Processor"],
            "UnlockItem": {"ConfigName": "MyMachine", "ConfigType": "ConstructOptions"}}},
        "DT_EnemyCrafting": {
            "WoodGear": {"ProductInfo": {"Count": 4}},
            "PlankNails": {"IngredientList": [{"IngredientName": "WoodBoard", "Count": 1}],
                "ProductInfo": {"IngredientName": "Nails", "Count": 5}, "CraftType": "Process",
                "CraftingTime": 2.0}}
    });
    let sources = [Source {
        label: "test.json".into(),
        contents,
    }];
    let built = build(&edit_game(&schemas, &assets), &sources).unwrap();
    assert_eq!(built.changed["DT_Buildings"]["MyMachine"]["ID"], 220);
    assert_eq!(
        built.changed["DT_EnemyCrafting"]["WoodGear"]["ProductInfo"]["Count"],
        4
    );
    assert_eq!(built.tables.len(), 3);
    assert_eq!(
        built.references["/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C"],
        "/Script/Engine.BlueprintGeneratedClass"
    );
    let warnings = built.warnings.join("\n");
    assert!(
        warnings.contains("PlankNails: no research unlocks it"),
        "{warnings}"
    );
}

/// Mistakes in edit files are all reported, each with where it is.
#[test]
#[ignore]
fn edit_errors() {
    use alchemists_core::edits::{build, Source};
    let Some((_, schemas, assets)) = game() else {
        return;
    };
    let contents = serde_json::json!({
        "DT_EnemyCrafting": {
            "WoodGear": {"CraftingTim": 3},
            "Mortar": {"IngredientList": [{"IngredientName": "Plank", "Count": 1}]},
            "LinseedOil": {"CraftType": "Mixing"},
            "HealingPotion": {"ProductInfo": {"Count": 2.5}},
            "NewThing": {"IngredientList": [], "CraftType": "Assemble"}},
        "DT_Buildings": {"NoSuchRow": {"ID": 3}, "Copy": {"_copy": "Missing"}},
        "DT_Nope": {"X": {}}
    });
    let sources = [Source {
        label: "bad.json".into(),
        contents,
    }];
    let error = format!(
        "{:#}",
        build(&edit_game(&schemas, &assets), &sources)
            .err()
            .unwrap()
    );
    for expected in [
        "unknown field 'CraftingTim'",
        "unknown item 'Plank'",
        "'Mixing' is not one of",
        "expected a whole number, got 2.5",
        "new recipes need ProductInfo, CraftingTime",
        "DT_Buildings.NoSuchRow: row not found",
        "_copy row 'Missing' not found",
        "DT_Nope: unknown table",
    ] {
        assert!(error.contains(expected), "no {expected:?} in:\n{error}");
    }
}

/// Components of game Blueprints, as Python's decoder reads them (NOTES.md in the mod project).
#[test]
#[ignore]
fn blueprint_components() {
    use alchemists_core::blueprint::{assets as top_level, components};
    use alchemists_core::unpack::legacy_assets;
    let Some((game, schemas, _)) = game() else {
        return;
    };
    let wanted = ["BP_Processor.uasset", "BP_PhantomTemporaryBag.uasset"];
    let found = legacy_assets(
        &game.paks(),
        |path| {
            path.starts_with("AlchemyFactory/Content/Blueprints/Buildings/")
                && wanted.iter().any(|w| path.ends_with(w))
        },
        |_| true,
    )
    .unwrap();
    let path = "AlchemyFactory/Content/Blueprints/Buildings/BP_Processor";
    let (uasset, uexp) = found.asset(path).unwrap();
    let parts = components(uasset, uexp, &schemas).unwrap();
    for part in &parts {
        println!(
            "{} ({}) in {:?}: {}",
            part.name, part.class, part.parent, part.values
        );
    }
    let body = parts.iter().find(|part| part.name == "StaticMesh").unwrap();
    assert_eq!(body.class, "StaticMeshComponent");
    assert!(body.values["StaticMesh"]
        .as_str()
        .unwrap()
        .ends_with("SM_Processor_1"));
    let gear = parts.iter().find(|part| part.name == "MainGear").unwrap();
    assert_eq!(gear.parent.as_deref(), Some("StaticMesh"));
    let crafting = parts
        .iter()
        .find(|part| part.name == "CraftFacility")
        .unwrap();
    assert_eq!(crafting.values["FactoryCraftType"], "Process");
    let assets = top_level(uasset).unwrap();
    assert_eq!(
        assets["/Game/Blueprints/Buildings/BP_Processor.BP_Processor_C"],
        "/Script/Engine.BlueprintGeneratedClass"
    );
    let (uasset, uexp) = found
        .asset("AlchemyFactory/Content/Blueprints/Buildings/BP_PhantomTemporaryBag")
        .unwrap();
    let bag = components(uasset, uexp, &schemas).unwrap();
    let container = bag
        .iter()
        .find(|part| part.name == "IngredientContainer")
        .unwrap();
    assert_eq!(container.values["capacity"], 30);
}

/// MyMachine's cooked Blueprint (in the stub project, if it's on this machine) packs and checks,
/// with its craft type set past Max.
#[test]
#[ignore]
fn content_my_machine() {
    use alchemists_core::content::{craft_type_number, package, patch_craft_type, stage};
    let Some((game, schemas, _)) = game() else {
        return;
    };
    let Some(home) = std::env::var_os("USERPROFILE") else {
        return;
    };
    let project = std::path::PathBuf::from(home)
        .join("Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject");
    if !project.exists() {
        eprintln!("no stub project: skipping");
        return;
    }
    let mut staged = stage(&project, &["/Game/Mods/MyMachine".to_string()]).unwrap();
    let wanted = craft_type_number("26", &schemas).unwrap();
    patch_craft_type(&mut staged[0], wanted).unwrap();
    let craft_types = indexmap::IndexMap::from([("BP_MyMachine".to_string(), wanted)]);
    let utoc = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("content/zzz_Content_P.utoc");
    let packaged = package(&utoc, &staged, &game, &schemas, &craft_types).unwrap();
    let parts = &packaged.components["/Game/Mods/MyMachine/BP_MyMachine"];
    assert!(parts
        .iter()
        .any(|part| part.name == "MainGear" && part.parent.as_deref() == Some("StaticMesh")));
    assert_eq!(
        packaged.assets["/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C"],
        "/Script/Engine.BlueprintGeneratedClass"
    );
}

/// The template save: read, edited as QuickStart edits it, and refused edits.
#[test]
#[ignore]
fn template_save_edits() {
    use alchemists_core::save::{edit_save, read_save, SaveEdit, TEMPLATE};
    use alchemists_core::unpack::pak_file;
    let Some((game, _, _)) = game() else { return };
    let template = pak_file(&game.pak(), TEMPLATE).unwrap();
    let (header, body) = read_save(&template).unwrap();
    assert_eq!(header["Gold"], 0);
    assert_eq!(body.len(), 134_492);
    let edit = |property: &str, after: &str, expect: i64, value: i64| SaveEdit {
        property: property.into(),
        value: value.into(),
        occurrence: 0,
        after: Some(after.into()),
        expect: Some(expect.into()),
        part: None,
    };
    let edits = [
        edit("Count", "RequireProducts", 20, 10),
        edit("Count", "Products", 200, 500),
    ];
    let (edited, old) = edit_save(&template, &edits).unwrap();
    assert_eq!(old, [serde_json::json!(20), serde_json::json!(200)]);
    let (_, new_body) = read_save(&edited).unwrap();
    let changed: Vec<usize> = (0..body.len())
        .filter(|&i| body[i] != new_body[i])
        .collect();
    assert!(changed.len() <= 4, "{} bytes changed", changed.len());
    std::fs::write(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("DefaultGame.sav"),
        &edited,
    )
    .unwrap();

    let error = edit_save(&template, &[edit("Count", "RequireProducts", 25, 10)]).unwrap_err();
    assert!(error.to_string().contains("not the expected 25"), "{error}");
    let name = SaveEdit {
        property: "QuestId".into(),
        ..edit("x", "y", 0, 3)
    };
    let name = SaveEdit {
        after: None,
        expect: None,
        ..name
    };
    let error = format!("{:#}", edit_save(&template, &[name]).unwrap_err());
    assert!(error.contains("only numbers and true/false"), "{error}");
}

/// A save made since the 2026-10-16 update (body version 3), if there is one on this machine.
#[test]
#[ignore]
fn version_3_save() {
    let Some(home) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let saves = std::path::PathBuf::from(home).join("AlchemyFactory/Saved/SaveGames");
    let Some(save) = std::fs::read_dir(&saves)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("Test_v1"))
        .map(|entry| entry.path().join("SaveGame0.sav"))
        .find(|path| path.exists())
    else {
        eprintln!("no Test_v1 save: skipping");
        return;
    };
    let (header, body) = alchemists_core::save::read_save(&std::fs::read(save).unwrap()).unwrap();
    assert_eq!(header["Name"], "Test_v1");
    assert!(body.len() > 100_000);
}

/// The include fixer gives the same module as the Python one did for the stub project
/// (`<project>/Source/BeltTD`, apart from BeltTD.Build.cs's two fixes), if both are on this
/// machine.
#[test]
#[ignore]
fn header_dump_includes() {
    let Some((game, _, _)) = game() else { return };
    let Some(home) = std::env::var_os("USERPROFILE") else {
        return;
    };
    let stub = std::path::PathBuf::from(home)
        .join("Documents/Unreal Projects/AlchemyFactory/Source/BeltTD");
    let engine = std::path::Path::new("C:/Program Files/Epic Games/UE_5.7/Engine");
    if !stub.is_dir() || !engine.is_dir() {
        eprintln!("no stub project or engine: skipping");
        return;
    }
    let dump = game
        .root
        .join("AlchemyFactory/Binaries/Win64/ue4ss/UHTHeaderDump/BeltTD");
    let copy = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("BeltTD");
    let _ = std::fs::remove_dir_all(&copy);
    let mut todo = vec![dump.clone()];
    while let Some(folder) = todo.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            let target = copy.join(path.strip_prefix(&dump).unwrap());
            if path.is_dir() {
                todo.push(path);
            } else {
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::copy(&path, &target).unwrap();
            }
        }
    }
    let changed = alchemists_core::update::fix_includes(&copy, engine).unwrap();
    assert!(changed > 100, "only {changed} files changed");
    let mut differing = vec![];
    let mut todo = vec![copy.clone()];
    while let Some(folder) = todo.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
                continue;
            }
            let relative = path.strip_prefix(&copy).unwrap();
            if relative.ends_with("BeltTD.Build.cs") {
                continue;
            }
            if std::fs::read(&path).ok() != std::fs::read(stub.join(relative)).ok() {
                differing.push(relative.display().to_string());
            }
        }
    }
    assert!(
        differing.is_empty(),
        "{} differ, e.g. {:?}",
        differing.len(),
        &differing[..differing.len().min(5)]
    );
}
