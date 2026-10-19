# The stub Unreal project

The game loads only *cooked* assets, and only Unreal Engine cooks them. The stub project is an empty Unreal
project set up to look like the game where it matters:
- it has the game's own C++ classes (`CraftFacilityComponent`, `DynamicFurnitureActor`, …), so a Blueprint
  made in it can be a real machine;
- it has the game's collision settings;
- it has the game's folder root.

You make Blueprints, meshes, textures and material instances in it, cook them, and the tools package the
cooked files as a mod.

You need it only for cooked content: Blueprints (`docs/examples/blueprints.md`), and models and icons
(`docs/examples/cooked-content.md`). Table edits, template-save edits and Lua mods don't need it.

## What it contains

| Part | Why |
|---|---|
| The project name `AlchemyFactory` | Cooked files land under `AlchemyFactory/Content/…`, the only root the game loads from |
| Unreal Engine 5.7.4 (changelist 51494982) | The game's exact engine build. Cooked files from another version may not load |
| The C++ module `BeltTD`, from a UE4SS header dump | The game's classes, so a Blueprint's parent and components are the game's own |
| The game's collision channels and profiles, in `Config/DefaultEngine.ini` | The presets machines use (`Furniture`, `FurnitureNoBuild`, …), with the same channel numbers |
| `/Game/Mods` and `/Game/Blueprints` cooked always, in `Config/DefaultGame.ini` | Otherwise the cook takes only what a map references, and no map references a mod |
| Stand-ins under `Content/Arts/` | Placeholders at the paths of the game's own models and materials (below) |

The tools look for it in `Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject`. Give
`--project <path to the .uproject>` to use another place.

## Setting it up

You do this once. After a game update that changes the game's C++, `alchemists-tools update` redoes
what's needed (below).

**1. Install the tools.**
- **Unreal Engine 5.7** from the Epic Games Launcher. Check that it's 5.7.4, then turn off automatic
  updates for it, so it stays the game's build.
- **Visual Studio 2022** with the *Game development with C++* workload, MSVC 14.44, and the .NET Framework
  4.8 SDK (Unreal's build needs it).
- **UE4SS** in the game (`AlchemyFactory/Binaries/Win64/`), for the header dump. The tools need it anyway,
  for the game's `.usmap`.

**2. Make the project.**
1. In the Launcher, create a **blank C++** project, with no starter content.
2. Name it exactly `AlchemyFactory`, and put it in `Documents/Unreal Projects/`.
3. Close the editor once it has opened.

**3. Dump the game's classes.**
1. Start the game with UE4SS, and begin a brand new save; the tutorial loads every class the tools need.
2. Once it has loaded, press **Ctrl + Numpad 9**.
3. Before closing the game, check that `Binaries/Win64/ue4ss/UHTHeaderDump/BeltTD/` has a `Public/`
   folder of `.h` files. An incomplete dump has only `.cpp` files.

Press **Ctrl + Numpad 6** too: that's the `.usmap` every tool uses.

**4. Set it up and build it:**
```
alchemists-tools stub
```
(`--project <path to the .uproject>` for a project elsewhere, `--engine <folder>` for an engine elsewhere.)

It does the rest, and says what it changed:
1. **The game's module**: copies the header dump to `Source/BeltTD/`. UE4SS leaves a comment
   (`//CROSS-MODULE INCLUDE V2`) where an engine header belongs, so the command finds each header and
   writes the `#include`. It also fixes two lines of `BeltTD.Build.cs` (a setting renamed in Unreal 5.6,
   and a missing `OnlineSubsystem` dependency).
2. **Registering it**: adds the module `BeltTD` to `AlchemyFactory.uproject` and to both `Target.cs`
   files, and enables the plugins it needs (`CommonUI`, `EnhancedInput`, `OnlineSubsystemUtils`) for
   every target, not only the editor.
3. **Collision**: copies the `[/Script/Engine.CollisionProfile]` section of the game's own
   `DefaultEngine.ini`, from its `.pak`, into the project's.
4. **Cooking**: adds `/Game/Mods` and `/Game/Blueprints` to the folders `DefaultGame.ini` always cooks.
   That's *Project Settings → Packaging → Additional Asset Directories to Cook* in the editor.
5. **Building** the editor target (about 2½ minutes the first time). `--no-build` leaves that for later.

Each step only changes what isn't done yet, so it's safe to run again. Once `Source/BeltTD/` exists it
isn't replaced: after a game update, `alchemists-tools update` replaces it if the header dump changed. It
keeps a backup of the old module and rebuilds.

**5. Check it.**
1. Open `AlchemyFactory.uproject`. If it asks to rebuild modules, choose *Yes*.
2. In the Content Browser's *Settings*, tick *Show C++ Classes* and *Show Engine Content*.
3. Check that *C++ Classes → BeltTD* lists the game's classes.

## How the project is laid out

```
Content/
  Mods/<ModName>/        your assets: Blueprints, meshes, textures, material instances. Packaged.
  Arts/...               stand-ins for the game's own assets. Never packaged.
  Blueprints/...         only for tests that replace a game Blueprint (--allow-game-paths)
```

- **Your assets go in `Content/Mods/<ModName>/`**, which is `/Game/Mods/<ModName>/` in the game. Each mod
  has its own folder, so no two mods' assets share a path. A mod's `mod.json` names the folder in
  `content`.
- **Stand-ins** let your assets use the game's. To give a Blueprint the Processor's model, make
  placeholders where the game keeps it:
  - placeholder meshes at `Content/Arts/Mesh/Facilities/SM_Processor_1` (and `_2`, `_3`);
  - a placeholder material instance at `Content/Arts/Material/Facilities/MI_AdvancedAthanorRotator`.

  The cooked Blueprint then refers to those paths, and in game the real assets load. A stand-in can be
  anything of the right kind, such as a copy of the engine's cube. Its folder and name must match the
  game's exactly (`Material`, not `Materials`). FModel, pointed at the game's `Paks` folder with the
  `.usmap`, shows the game's paths.
- **Never package a stand-in.** It would replace the real asset everywhere in the game. The packager
  refuses anything under `/Game/Arts`.

## Cooking

In the editor, choose *Platforms → Windows → Cook Content*. A notification says when it's done, and
*Window → Output Log* shows the details. The first cook takes a while.

Without the editor:
```
"C:\Program Files\Epic Games\UE_5.7\Engine\Binaries\Win64\UnrealEditor-Cmd.exe" "<project>\AlchemyFactory.uproject" -run=cook -targetplatform=Windows
```

The cooked files are in `<project>/Saved/Cooked/Windows/AlchemyFactory/Content/`. **Cook again after every
save in the editor**: the build stops when an asset was saved after its last cook, or was never cooked.

## From cooked files to a mod

A mod folder's `mod.json` lists its cooked folders:
```json
{"name": "MyMachine", "content": ["/Game/Mods/MyMachine"]}
```
`alchemists-tools mod build MyMachine` (or `install`) then:
1. packages the cooked files as the mod's content container (`zzz_MyMachineContent_P`);
2. checks the container next to the game's own, so every reference must resolve to a real asset;
3. lists each Blueprint's components.

`alchemists-tools package-content /Game/Mods/MyMachine --name MyMachineContent` does the packaging alone.
Its `--craft-type BP_MyMachine=26` sets a craft type the editor can't offer; in a mod folder that's
`"craft-types"` in `mod.json`.

## If something goes wrong

- **`DynamicFurnitureActor` isn't offered as a parent class**: *C++ Classes → BeltTD* is missing, so the
  module didn't load. Run `alchemists-tools stub` again, and read the build's errors.
- **The build fails in `BeltTD`** after a game update: run `alchemists-tools update`, which re-copies
  the dump and fixes its includes. Check that the dump has a `Public/` folder (step 3).
- **`Furniture` and `FurnitureNoBuild` aren't in *Collision Presets***: restart the editor. It reads
  `DefaultEngine.ini` only when it starts.
- **The check says a reference doesn't resolve**: the asset points at a path the game doesn't have.
  Usually that's a stand-in in the wrong folder or with the wrong name.
- **The game crashes or shows nothing**: check that the editor is 5.7.4, and that no stand-in was
  packaged.
