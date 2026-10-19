# Worked example: a machine with its own Blueprint

The worked example is `examples/MyMachine/` (in this repo). It's a new machine with its own build-menu entry, made in the
stub project:
- it has the Processor's model;
- it runs the Assembler's recipes;
- it's named "My Machine".

The second half of this page gives it a recipe list of its own.

| Part | Status |
|---|---|
| A Blueprint made in the stub project, as a new building | Confirmed in game: placed, clicked, crafted |
| Its own name, through the merged tables | Confirmed in game |
| A craft type of its own (`Max`, or a number from 26) | Confirmed in game for `Max` and 26, set on a placed machine by a Lua probe |
| `new-craft-types` in `mod.json` | Builds; not tried in game |

You need the stub project, set up as in `docs/STUB-PROJECT.md`.

## What a machine is

1. **A row of `DT_Buildings`**: cost, name, icon, footprint, ports, and `ScriptClass`, the Blueprint to
   spawn.
2. **That Blueprint**: the model and the parts. The crafting itself is the game's C++ component,
   `CraftFacilityComponent`, so the Blueprint only has to put the right parts together.

The game's Processor Blueprint, read from the game files:

| Component | Class | Parent | Settings |
|---|---|---|---|
| `DefaultSceneRoot` | `SceneComponent` | (root) | none |
| `StaticMesh` | `StaticMeshComponent` | `DefaultSceneRoot` | mesh `SM_Processor_1`; rotation Z 180; scale 0.99; collision `FurnitureNoBuild`; component tag `FixedMesh` |
| `MainGear` | `StaticMeshComponent` | `StaticMesh` | mesh `SM_Processor_2`; location (0, −25, 25); material `MI_AdvancedAthanorRotator`; collision `Furniture` |
| `Gear1` | `StaticMeshComponent` | `StaticMesh` | mesh `SM_Processor_3`; location (14.267, −12.952, 13.574); rotation Y 3; same material and collision |
| `Gear2` | `StaticMeshComponent` | `StaticMesh` | mesh `SM_Processor_3`; location (−18.036, −12.754, 25.054); rotation Y −2; same material and collision |
| `CraftFacility` | `CraftFacilityComponent` | (root) | `FactoryCraftType`: `Process` |

Its parent class is the game's `DynamicFurnitureActor`. Its own logic only turns the gears.

## 1. Stand-ins for the model

The Blueprint uses the game's real model through stand-ins: placeholders at the game's paths
(`docs/STUB-PROJECT.md`). In the Content Browser:
1. Make `Content/Arts/Mesh/Facilities/` and `Content/Arts/Material/Facilities/`.
2. Copy the engine's *BasicShapes → Cube* into the first, as `SM_Processor_1`, `SM_Processor_2` and
   `SM_Processor_3`.
3. In the second, make a *Material Instance* called `MI_AdvancedAthanorRotator`.
4. *File → Save All*.

They're never packaged; in game the real ones load.

## 2. The Blueprint

1. Make the folder `Content/Mods/MyMachine/`.
2. In it, choose *Blueprint Class*. Under *All Classes*, search for `DynamicFurnitureActor`, and select it.
3. Name the Blueprint `BP_MyMachine`, and open it.
4. Add the components in the table above. Select a component before *+ Add* to make the new one its child:
   - the body: select `DefaultSceneRoot`, then add a *Static Mesh* named `StaticMesh`;
   - the three gears: select `StaticMesh`, then add them;
   - the crafting: select `DefaultSceneRoot`, then add a *Craft Facility* named `CraftFacility`. Set its
     *Factory Craft Type* to `Assemble`.

   The editor shows rotations as X roll, Y pitch, Z yaw. If the body was scaled first, the gears show
   1.0101: type 1 into each scale box.
5. Leave the *Event Graph* empty. The gears won't turn, but the machine works.
6. *Compile*, *Save*, then cook (*Platforms → Windows → Cook Content*).

## 3. The mod folder

`examples/MyMachine/mod.json` names the cooked folder:

```json
{
  "name": "MyMachine",
  "title": "My Machine",
  "version": "0.1.0",
  "content": ["/Game/Mods/MyMachine"]
}
```

`examples/MyMachine/edits/my-machine.json` adds the building row, pointing at the Blueprint, and its
research:

```json
{
  "DT_Buildings": {
    "MyMachine": {
      "_copy": "Processor",
      "ID": 220,
      "ScriptClass": "/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C",
      "AlterBuildOption": "None",
      "CostList": [{"IngredientName": "WoodBoard", "Count": 1}],
      "DisplayName": "My Machine",
      "DisplayDesc": "A test machine: an Assembler in a Processor's body."
    }
  },
  "DT_SkillPoints": {
    "MyMachine": {
      "_copy": "Processor",
      "SlotPosition": {"X": -2, "Y": 4},
      "Predecessors": ["Processor"],
      "UnlockItem": {"ConfigName": "MyMachine", "ConfigType": "ConstructOptions"}
    }
  }
}
```

- **`ScriptClass`** is the Blueprint's class: its path, a dot, and its name with `_C`.
- **The copied row** keeps the Processor's footprint and ports (one belt in, one out). The Blueprint's
  model must fit them. For another shape, copy a building with that shape, or copy its `GridConfigList`
  and `InOutList`.
- The rest is as for any new building (`docs/examples/buildings.md`).

## 4. Build, install, test

```
alchemists-tools mod build MyMachine --mods examples
alchemists-tools mod install MyMachine --mods examples
```

The build:
- stops if the Blueprint was saved after its last cook;
- packages the cooked folder as `zzz_MyMachineContent_P`;
- checks that every reference resolves to a real asset;
- lists the components, to compare with what you meant:

```
== cooked content (/Game/Mods/MyMachine)
   /Game/Mods/MyMachine/BP_MyMachine:
      CraftFacility: craft type Assemble
      DefaultSceneRoot: SceneComponent
      Gear1 (in StaticMesh): SM_Processor_3, material MI_AdvancedAthanorRotator, collision Furniture, ...
```

In a new or Sandbox save:
1. Research My Machine.
2. Build it from its own entry, last among the active crafting machines.
3. Check that it looks like a Processor and offers the Assembler's recipes.

Deconstruct it before uninstalling (`docs/examples/buildings.md`).

## 5. A recipe list of its own

A machine runs the recipes whose `CraftType` matches its Blueprint's `FactoryCraftType`. All 25 named craft
types belong to the game's machines, so a machine of its own needs another value:
- `Max`, which the editor can offer and no recipe uses;
- or a number from 26 to 255.

A mod names its craft types, and the tools give each one a number:

```json
{
  "name": "MyMachine",
  "content": ["/Game/Mods/MyMachine"],
  "new-craft-types": ["Sorting"],
  "craft-types": {"BP_MyMachine": "MyMachine:Sorting"}
}
```

- **`new-craft-types`** gives `MyMachine:Sorting` the first free number from 26. It's kept in
  `mods/allocations.json`, so it never changes once given. Keep that file, and share it with the mod's
  source.
- **`craft-types`** writes the number into the cooked Blueprint's `FactoryCraftType`, which the editor
  can't do.

Recipes then use the name:

```json
{
  "DT_EnemyCrafting": {
    "SortPlanks": {
      "IngredientList": [{"IngredientName": "WoodBoard", "Count": 1}],
      "ProductInfo": {"IngredientName": "WoodGear", "Count": 2},
      "CraftType": "MyMachine:Sorting",
      "CraftingTime": 5.0
    }
  },
  "DT_SkillPoints": {
    "SortPlanks": {
      "_copy": "WoodGear",
      "SlotPosition": {"X": -3, "Y": 5},
      "Predecessors": ["MyMachine"],
      "UnlockItem": {"ConfigName": "SortPlanks", "ConfigType": "CraftingRecipes"}
    }
  }
}
```

The build reports `new craft type MyMachine:Sorting = 26` and `craft type 10 -> 26 in BP_MyMachine`. It
warns when a craft type has no machine or no recipes. Another mod can add recipes for this machine by
depending on `MyMachine` and using the same name.

Known limits of an own craft type:
- the research tooltip shows a blank machine symbol and no machine name;
- the game's `IsCraftTypeUnlocked` is always false for it (as it already is for the Alembics, which work).

## Replacing a game Blueprint

A Blueprint cooked at a game path replaces the game's. Cooked at
`/Game/Blueprints/Buildings/BP_Processor`, it made every Processor an Assembler (confirmed in game). That's
useful for tests, but it replaces the Blueprint for every player and every other mod. Packaging it needs
`--allow-game-paths`. Prefer new rows with Blueprints of their own.

## Things to know

- **The model comes from stand-ins** unless you make your own (`docs/examples/cooked-content.md`).
- **The gears don't turn**: the game's turning logic hasn't been rebuilt.
- **Never package a stand-in**, and never rename or remove a released building row.
- **After a game update**, `alchemists-tools update` compares the game Blueprints that mods copy with the
  last snapshot, and reports any change.
