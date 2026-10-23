# Making mods

A mod is a folder of plain files:
- JSON edits to the game's data tables;
- edits to the save new games start from;
- Lua scripts;
- assets cooked in Unreal.

`alchemists-tools` builds it into files the game loads, checks everything it can, and installs it next to
other mods. The game's own files are never changed.

- [Quick start](#quick-start)
- [Edit files](#edit-files)
- [New rows](#new-rows)
- [Mod folders](#mod-folders)
- [Building](#building)
- [Installing](#installing)
- [The template save](#the-template-save)
- [Lua mods](#lua-mods)
- [Cooked content](#cooked-content)
- [Sharing a mod](#sharing-a-mod)
- [After a game update](#after-a-game-update)

## Quick start

1. Dump the game's mappings once (the README, "Dump the game's mappings").
2. Extract the game's tables, to see what's there:
   ```
   alchemists-tools extract
   ```
3. Start a mod:
   ```
   alchemists-tools mod new MyMod
   ```
   This makes `mods/MyMod/` with a `mod.json` and `edits/example.json`. The example applies nothing
   until you remove the `_` from a table's name, and the build warns about each table it skips that way.
4. Put your edit files in `mods/MyMod/edits/`.
5. Build and install, then restart the game:
   ```
   alchemists-tools mod install MyMod
   ```

To remove it: `alchemists-tools mod uninstall MyMod`.

Try mods in a new save, or a copy of one. Saves are in `%LOCALAPPDATA%/AlchemyFactory/Saved/SaveGames/`.

## Edit files

Every `*.json` file in a mod's `edits/` folder is read, in alphabetical order. An edit file is an object of
**table → row → fields to change**:

```json
{
  "DT_EnemyCrafting": {
    "WoodGear": {
      "_comment": "Processor: 1 Large Wooden Gear -> 4 Small Wooden Gears",
      "ProductInfo": {"Count": 4}
    }
  },
  "DT_PlantSeedConfig": {
    "FlaxSeed": {"GrowthNutrientValue": 12.0}
  }
}
```

The rules:

- **Tables, rows and fields** are as `alchemists-tools extract` writes them: the table is the file name in
  `data/DataTables/`, the rows are the keys under `"rows"`, and the fields are the keys of a row. Row names
  are the game's IDs, such as `WoodBoard` for Planks; `DisplayName` gives the English name.
- **Only give what changes.** Objects merge field by field; anything you leave out keeps the game's value.
- **Lists are replaced whole.** The list you give becomes the new list, so give every entry with every
  field. Lists can be longer or shorter than the original.
- **Item and row names** are IDs, not display names. A wrong name stops the build, which suggests close
  matches.
- **Comments:** a key starting with `_` is skipped, apart from the row options below. The build warns
  when a skipped table or row holds rows or fields, so a forgotten `_` doesn't go unnoticed.
- **Several files:** when two files change the same field, the later file wins.

Values must match the field's type:

| Type | Written as |
|---|---|
| Whole and decimal numbers, true/false | `4`, `4.0`, `true` |
| Names (rows of other tables) | `"IngredientName": "Stone"` |
| Choices (enums) | their name: `"CraftType": "Blend"` |
| Texts | any string: `"DisplayName": "My Machine"`. A new text is the same in every language. A text you leave alone, including on a copied row, keeps the game's translations |
| Assets | a path: `"ScriptClass": "/Game/Mods/MyMachine/BP_MyMachine.BP_MyMachine_C"` |

**Asset references.** An asset path the table doesn't refer to yet is added to it. That can be a
Blueprint class, a texture (`DisplayIcon`), a mesh, or any other asset. The asset itself has to be packaged
too, which a mod folder does (Cooked content, below). Each of the game's asset fields holds one class
(`DisplayIcon` is always a `Texture2D`), so the class comes from the field. For a field the game leaves
empty everywhere, write the class too, as the editor's *Copy Reference* gives it:
`"/Script/Engine.Texture2D'/Game/Mods/MyMod/T_Icon.T_Icon'"`.

The build stops when:
- a class doesn't match the field;
- a game asset doesn't exist (a misspelt `/Game/Arts/...` path);
- a `/Game/Mods/` asset isn't packaged by the mod or a mod it requires, or has another class.

It lists each new reference.

## New rows

### New recipes

A row name that `DT_EnemyCrafting` doesn't have makes a new recipe. Give at least `IngredientList`,
`ProductInfo`, `CraftType` (which machines run it) and `CraftingTime`:

```json
{
  "DT_EnemyCrafting": {
    "PlanksToLargeGear": {
      "IngredientList": [{"IngredientName": "WoodBoard", "Count": 2}],
      "ProductInfo": {"IngredientName": "LargeWoodGear", "Count": 1},
      "CraftType": "Process",
      "CraftingTime": 8.0
    }
  }
}
```

- Any other recipe field can be given too.
- **Defaults:** no side or fail products, `FractionNum` 1, visible, a new unique `ID`, and `bAlternate`
  true if another recipe already makes the item.
- **`"_slot": "<row>"`** writes the recipe over one of the game's unused recipe rows instead of adding a
  row. The build lists the free ones.
- **Don't rename a recipe that's in use.** Saves remember a machine's recipe by its row name.

### Unlocking with research

A new recipe or building can't be used until a research unlocks it. Add a row to `DT_SkillPoints`,
starting from a copy of an existing research:

```json
{
  "DT_SkillPoints": {
    "PlanksToLargeGear": {
      "_copy": "WoodGear",
      "SlotPosition": {"X": -1, "Y": 5},
      "Predecessors": ["WoodGear"],
      "UnlockItem": {"ConfigName": "PlanksToLargeGear", "ConfigType": "CraftingRecipes"}
    }
  }
}
```

- **`SlotPosition`** is the node's place in the tree: X left and right, Y down. Put it in the row just
  below its predecessors: a link runs down the predecessor's column, then sideways, so a node in that
  column in between looks like where it comes from (the build warns about it). The build stops when two researches that
  aren't deprecated share a place.
- **`Predecessors`** are the researches needed first.
- **`UnlockItem`** is what it unlocks: a recipe (`CraftingRecipes`) or a building (`ConstructOptions`).
- **Cost and time** (`UnlockCost`, `UnlockResearchPoints`) are copied from the `_copy` row unless you
  give them.

### Other new rows

Any table can get new rows with `"_copy": "<existing row>"`: the new row starts as a copy, then your fields
are applied.

A new `ID` is assigned if the table has one and you don't give it. Building IDs group buildings in the
build menu (2xx are active crafting machines), so a copied building gets the first free ID in the hundred
of the one it copies. Other tables count on from their highest ID.

The build stops when a changed row shares an ID with another row.

**A save with a placed building whose row is missing crashes on load.** So players must deconstruct a
mod's buildings before uninstalling it, and a building row must never be renamed or removed once
released. The install, the uninstall and the README `dist` writes all warn about each new building.

## Mod folders

```
mods/MyMod/
  mod.json                 name, version, dependencies, cooked content, craft types
  thumbnail.png            its picture in the GUI (or .jpg; any size, shown square)
  edits/*.json             table edits
  edits/if-Other/*.json    table edits applied only when the mod Other is installed too
  save/DefaultGame.json    edits to the save new games start from
  ue4ss/                   a UE4SS mod: Lua (Scripts/main.lua) or C++ (dlls/main.dll)  -> ue4ss/Mods/MyMod/
  ue4ss-shared/MyModX/     Lua modules for other mods          -> ue4ss/Mods/shared/MyModX/
```

`mod.json`:

```json
{
  "name": "MyMachine",
  "title": "My Machine",
  "version": "0.1.0",
  "description": "A second Assembler.",
  "author": "",
  "homepage": "",
  "dependencies": ["AlchemistsLibrary >= 0.2.4", "? BiggerChests", "! OtherStorageMod"],
  "game-build": "25991404",
  "content": ["/Game/Mods/MyMachine"],
  "new-craft-types": ["Sorting"],
  "craft-types": {"BP_MyMachine": "MyMachine:Sorting"}
}
```

Only `name` is needed. Like Factorio's `info.json`:
- **`name`**: letters and digits, starting with a letter. It names the containers and the UE4SS folder.
- **`title`**: the name players see (default: `name`).
- **`version`**: `major.minor.patch` (default `0.1.0`).
- **`description`, `author`, `homepage`, `contact`**: shown in the README `dist` writes.
- **`team`**: the Thunderstore team that publishes it: letters, digits and `_`, not starting or ending with `_`.
  Mods that require it name it with this in their `manifest.json` (see "Sharing a mod").
- **`dependencies`**: other mods, written `"[prefix] Name [operator version]"`. The operators are `<`,
  `<=`, `=`, `>=` and `>`.

  | Prefix | Meaning |
  |---|---|
  | none | required: must be installed, and loads first |
  | `?` | optional: if installed, it loads first and must match the version, and `edits/if-<Name>/` apply |
  | `(?)` | the same as `?`, but not listed in the README |
  | `+` | recommended: mentioned when missing and listed in the README; doesn't change the load order |
  | `~` | required, but doesn't change the load order (for two mods that need each other) |
  | `!` | incompatible: neither installs while the other is installed |

The fields this tool adds:
- **`game-build`**: the game's Steam build the mod is made for (`alchemists-tools check` shows the installed one;
  `mod new` fills it in). When the game is newer, building warns, and the GUI and `mod list` mark the mod, as
  it may need updating. Once it's checked against the new build, change the number.
- **`detours`**: the game functions the mod's C++ code detours (replaces the start of), by their reflected
  paths, such as `"/Script/BeltTD.BeltTDInventoryComponent:CheckCanCost"`. Two mods can't both detour one
  function, so installing them together stops. `[]` says there are none. Without the field, a build whose
  `dlls/main.dll` names game functions warns. Functions hooked through UE4SS (Lua's `RegisterHook`, or named in
  the DLL) are found by the build; two unrelated mods hooking one game function is a warning. A native function
  with no reflected path (found by its address or its code) is named `exe+0x<offset>`, its offset in the game build
  of `game-build`, as in `"exe+0x4acbd60"`; prefer the reflected path when there is one, since a clash is only
  caught when both mods use the same name.
- **`content`**: folders or assets cooked in the stub project. They must be in `/Game/Mods/<name>/`, so no
  two mods share asset paths, and they're packaged as `zzz_<name>Content_P`.
- **`new-craft-types`**: recipe lists of the mod's own, past the game's named craft types. Edits and
  `craft-types` refer to them as `"<mod>:<name>"`, such as `"CraftType": "MyMachine:Sorting"`. Other mods
  can too, if they depend on this one. `mods/allocations.json` gives each one a number from 26, which
  never changes afterwards, so keep that file.
- **`craft-types`**: a Blueprint's `FactoryCraftType`, by name, number or `"<mod>:<name>"`, for values the
  editor can't offer.

## Building

```
alchemists-tools mod build MyMod         # -> build/MyMod/, laid out like the game folder
```

A mod is built on top of the mods it requires, so it can change their rows and use their items and craft
types. The build checks every table, row, field and value, and lists each change.

It stops when:
- an edit refers to a `/Game/Mods/` asset that no mod it builds on packages;
- a Blueprint was saved in the editor after its last cook, or was never cooked;
- a script requires another mod's Lua module without depending on that mod.

It warns about likely mistakes, judged from the game's own data:
- a recipe output (`SideProduct`, `FailProduct1/2`) that no game recipe for that machine uses. Machines
  that don't normally make side products discard them;
- an ingredient or product the machine has no connection for. Solid items need belt connections and
  liquids pipe connections; machines that sell their output, like the Brew Barrel, need no outputs;
- a research on a row level with or above its predecessor;
- a new building that no research unlocks;
- a craft type with no machine, or a machine's craft type with no recipes.

`build/<name>/build.json` records:
- the files and the game's Steam build;
- the new rows and buildings;
- the edits.

## Installing

```
alchemists-tools mod install MyMod [--with-dependencies] [--force]   # build, then install (and the mods it requires)
alchemists-tools mod uninstall MyMod [--force]
alchemists-tools mod apply MyMod Other [--with-dependencies] [--force]  # exactly these installed, all at once
alchemists-tools mod list                                    # mods, what's installed, and the load order
alchemists-tools mod scan                                    # every mod in the game folder, and its files
```

**Applying a set of mods.** `mod apply` (the GUI's Confirm button) makes the installed mods exactly the ones
named. Each is built and installed (again, if it's installed already), and every other installed mod is
uninstalled. Everything is built and checked before the game folder is touched: each build, the mods'
dependencies and incompatibilities as a set, the files each writes, and the merge (rows two mods add, functions
two mods detour). Any problem stops it with nothing changed, naming the mods it's about.

**All or nothing.** `apply`, `install` and `uninstall` change the game folder in one transaction. Each file is
moved aside before it's replaced or deleted (to `AlchemyFactory/.alchemists-transaction/`, where the game doesn't
look), with a journal written before each step. If a step fails, say because the running game holds a file,
everything is put back. If the tool itself is stopped part way, the next install or uninstall puts it back first.
They refuse to start while the game is running.

**The mods folder.** It holds the mods' sources, installed or not; the game folder only holds what's installed.
Save it once with `alchemists-tools settings --mods <folder>` (the GUI's Setup does the same); without a setting,
the command line uses `mods` where it's started. `--mods` can be given more than once, and a mod in more than one
folder is taken from the first. In a mods folder:
- a folder with a `mod.json` is a mod (a link to a folder elsewhere works too, such as a junction to a mod kept in
  its own repository);
- a git clone holds mods at its top or in its `mods/` folder;
- a zip holding a mod's manifest is a mod ready to install: one `mod dist` made, or one the tool kept.

**Kept mods.** Uninstalling a mod that no mods folder holds (one installed from its zip, or from a folder since
removed) first keeps it as `<name>_<version>.zip` in the first mods folder, with its files, its manifest and its
install record entry (which holds the table edits merged for it), so it stays listed and can be installed again.
A mod kept that way installs exactly as it was, but can't be rebuilt. If its files are missing, uninstalling stops,
unless forced (then no copy is kept).

**Mods from git.**

```
alchemists-tools mod add https://github.com/someone/SomeMods [--branch v1.2]   # clone into the mods folder
alchemists-tools mod update SomeMod                                           # pull it again
```

`mod add` clones the latest commit of a branch or tag. A repository that is one mod is named after it; one with
several is kept as it is, and its mods are found inside. Then install them as any other. `mod update` only
fast-forwards, so it never merges changes you made. Before adding a mod:
- a mod's Lua scripts and DLLs run in the game with your account's rights, so only add mods you trust;
- a mod is built from its source here, so the repository must hold everything it installs: a C++ mod's built
  `dlls/main.dll`, since it isn't compiled for you. A mod with cooked content (`content` in mod.json) can't be
  built without its author's stub project;
- the mods it requires aren't fetched: add their repositories too;
- it may be made for another game build (`game-build`), and its new craft types get numbers here
  (`allocations.json`), which may differ from its author's;
- git must be installed. Private repositories need credentials git already has (it isn't allowed to ask), and
  submodules and Git LFS files aren't fetched.

**Several mods together.** The game reads each table whole from the last container that has it. So
installing and uninstalling merge the table edits of every installed mod into one container,
`zzz_ModTables_P`, in load order: dependencies first, otherwise alphabetical. The merge:
- stops when two mods that don't depend on each other add the same row, or use the same unused recipe
  row;
- warns when they set the same field differently. The later one in the load order wins; make one depend
  on the other to decide.

`install` stops when a required mod is missing or the wrong version, or an incompatible mod is installed.
With `--with-dependencies`, it builds and installs the required mods found in `mods/` first. `uninstall`
stops when an installed mod requires the one being removed (`--force` to go on).

**What each mod owns.** Each installed mod has a manifest, `Paks/~mods/manifests/<name>.json`. It lists
the mod's name, version and dependencies, and every file it put in the game folder, with its size and
SHA-256 hash. `Paks/~mods/installed-mods.json` lists the installed mods, the load order, and the hashes of
the two files the tool merges for all of them (`zzz_ModTables_P` and `zzz_ModSave_P`).

No two mods may own the same file, so `install` stops when a file it would write:
- is in another mod's manifest, or is one of the merged files;
- already exists and isn't this mod's (a mod installed by hand, say), or has changed since this mod
  installed it.

Nothing is deleted or replaced unless all of this holds:
1. it's in that mod's manifest (or, for a merged file, recorded as written by the tool);
2. its path is relative, has no `..`, starts with `AlchemyFactory/`, and resolves inside the game folder,
   with links followed;
3. its size and hash are still those installed.

A file that has changed is kept, and `uninstall` lists it; `--force` deletes it too. A merged file that
has changed stops the install or uninstall before anything is touched; `--force` replaces it anyway. Folders are never deleted with
their contents: only files one by one, then the folders left empty below `ue4ss/Mods/`. So your own files
in a mod's folder keep it.

`uninstall` also needs `--force` for a mod without a manifest (installed by an older version, or by hand).
It lists the files it would delete first. Reinstalling the mod with this tool gives it a manifest.

`mod scan` reports:
- every mod with a manifest, and whether its files are as installed;
- mods recorded without a manifest;
- the merged files;
- everything in `Paks/~mods` and `ue4ss/Mods` that no mod accounts for, with the game assets each unknown
  container replaces;
- assets that more than one container replaces (only one copy takes effect).

## The template save

New games start as a copy of a save inside the game, `PreviewSaveGames/DefaultGame.sav`. It holds:
- the first quest;
- the starting inventory and unlocks;
- the loans;
- the starting land and buildings.

So table edits don't change those. A mod's `save/DefaultGame.json` edits the template's numbers and
true/false values:

```json
{
  "edits": [
    {"property": "Count", "after": "RequireProducts", "expect": 20, "value": 10}
  ]
}
```

- **`property`**: the value's name; the first one in the save, or the `occurrence`-th, counting from 0.
- **`after`**: count from the first value of this name instead. Above, that's the `Count` in the first
  quest's `RequireProducts`.
- **`expect`**: the current value. The build stops if it differs, after a game update for example, and
  says what's stored.
- **`value`**: the new value.
- **`in`**: `"header"` for the save's header (the summary in the load menu); the body otherwise.

Names, texts and list lengths can't be changed yet. The installed mods' edits are merged into
`zzz_ModSave_P`, and when two mods set the same value differently, the one later in the load order wins.
Template edits only affect new games.

## Lua mods

A mod's `ue4ss/` folder is a [UE4SS](https://docs.ue4ss.com/) Lua mod, installed to `ue4ss/Mods/<name>/`
and enabled. The tool's own library, AlchemistsLibrary, is a mod other mods can depend on: logging,
game-thread calls, key binds, and reading placed buildings and their contents. `alchemists-tools probe`
writes AlchemistsLibrary into the mods folder when it's missing. Use it with:

```lua
local Library = require("AlchemistsLibrary")
local log = Library.logger("MyMod")
log.log("hello")     -- ue4ss/UE4SS.log: [MyMod] hello
```

with `"dependencies": ["AlchemistsLibrary"]` in `mod.json`.

`alchemists-tools probe <Name> <Class>:<Function> ... [--new-object <class>]` writes a probe: a mod that
logs every call to those functions, with their arguments, and each new object of a class. It's the way
to find out what the game calls, and when.

The library's comments explain the rules:
- don't touch the world's objects in bulk while a world loads;
- use game objects on the game thread;
- in multiplayer, change the game only in ways every player's machine repeats exactly.

More rules, learned in game, are in `docs/examples/lua.md` ("Rules").

### C++ UE4SS mods

Lua can hook only what the game calls through the reflection system (Blueprints and the user interface). The
game's own C++ (building, charging costs, refunds, stack limits) calls itself directly, so changing that needs a
UE4SS C++ mod: a DLL that detours the game's native code. Put the built DLL at `ue4ss/dlls/main.dll` in the mod
folder; the tool installs it as it does a Lua mod, with the mod's table edits beside it. List the functions it
detours in mod.json's `"detours"`, so that a second mod detouring one of them can't be installed beside it.

Building one:
- it must be compiled against the source of the exact UE4SS build installed: `v3.0.1-1164-g5e627997`, the one
  the tools install for players (`core/src/ue4ss.rs`; the "Git SHA" on the second line
  of `ue4ss/UE4SS.log`), with Visual Studio 2022 (MSVC 14.43 or later), Rust and CMake;
- part of that source (UEPseudo) is private to GitHub accounts linked to an Epic Games account, with the
  invitation to the EpicGames organisation accepted;
- native code changes with game updates: find it at run time from the reflection data where possible
  (a reflected function's exec thunk leads to its C++ body), and switch the mod off if it isn't found.

[Alchemist's Vault](https://github.com/TobyTicehurst/alchemists-vault) is one, kept apart from this repository;
its `docs/research.md` explains the method.

## Cooked content

Blueprints, meshes, textures and material instances are made and cooked in a stub Unreal project.
`alchemists-tools stub` sets one up from a blank C++ project:
- it adds the game's classes, from the UE4SS header dump;
- it copies the game's collision settings;
- it sets the mod folders to cook;
- it builds the project.

A mod's `mod.json` lists its cooked folders in `content`. The build then:
1. packages them;
2. checks that every reference resolves to a real asset;
3. lists each Blueprint's components.

`alchemists-tools package-content /Game/Mods/MyMod --name MyModContent` packages content alone.

Assets live in `/Game/Mods/<name>/`. Placeholders at the paths of the game's own assets ("stand-ins",
under `/Game/Arts/`) let a Blueprint use the game's models. They're never packaged: the packager refuses
`/Game/Arts`.

## Sharing a mod

```
alchemists-tools mod dist MyMod      # -> build/MyMod_<version>.zip, and build/MyMod_<version>_README.md
```

One zip serves every way of sharing: upload it to Nexus Mods, or as a Thunderstore package, as it is. It holds:
- at its top, what Thunderstore reads: `manifest.json`, `icon.png` (the mod's `thumbnail.png` cut to 256x256, or a
  plain square), `README.md`, and the mod folder's `CHANGELOG.md` if it has one;
- `AlchemyFactory/...`: the mod's files as they go in the game folder, for players installing by hand, with its own
  data container (`zzz_<name>_P`, its edits plus those of the mods it requires) and its file manifest;
- `alchemists-tools-entry.json`: the mod as built, with its edits. A player with this tool puts the zip in their mods
  folder and ticks it, and its edits are merged with their other mods' (its own data container is left out).

`manifest.json` has Thunderstore's fields, and this tool's under a key of their own, so that neither clashes with
what Thunderstore adds later (Thunderstore ignores keys it doesn't know):

```json
{
  "name": "MyMod", "version_number": "1.0.0", "website_url": "", "description": "Up to 250 characters",
  "dependencies": ["MyTeam-AlchemistsLibrary-0.2.5"],
  "alchemists-tools": {
    "title": "My Mod", "description": "In full", "author": "", "team": "MyTeam", "contact": "",
    "dependencies": ["AlchemistsLibrary >= 0.2.5", "? BiggerChests"], "game-build": "25991404", "detours": []
  }
}
```

Thunderstore's `dependencies` are only the required mods, each as `<team>-<name>-<version>`, at the version the mod
was built against. Thunderstore checks that each one is on Thunderstore when the zip is uploaded, so publish
dependencies first. The team is the dependency's own mod.json `team`; a dependency without one is left out, with a
warning. This tool's `dependencies` keep the full form: version ranges, and optional, recommended and incompatible
mods, which Thunderstore can't express. `dist` checks Thunderstore's other rules: a plain version (`1.0.0`, not
`01.0.0`), a description of at most 250 characters (a longer one is cut short in `manifest.json` and kept whole in
the README), and a README of at most 100,000 characters.

The README, also written beside the zip for a mod page, has install and uninstall instructions for both kinds of
player, the dependencies, and a warning about new buildings. Since players installing by hand can't merge tables, it
also lists the tables the mod replaces, and which required mods' table files to delete.

## Mods made without this tool

```
alchemists-tools mod import "Some Mod v1.2.zip" [--name SomeMod] [--version 1.2.0]
```

The GUI's **Import a zip…** does the same. A mod from a mod site is turned, as far as can be done safely, into a zip
in the mods folder that installs and uninstalls beside other mods. This is a best effort for mods that exist
already; it isn't meant to last, and mods shared with `mod dist` need none of it.

The zip is never extracted as it is. Each file is checked and placed by rules:
- a UE4SS mod's folder (one holding `Scripts/` or `dlls/main.dll`, anywhere in the zip) goes to
  `ue4ss/Mods/<its name>/`: its Lua, `dlls/main.dll`, `enabled.txt` (added if missing) and text settings;
- containers (`.pak`, `.utoc`, `.ucas`) go to `Paks/~mods/`, or `Paks/LogicMods/` if they're in a `LogicMods` folder;
- documents and pictures are left out. So are UE4SS's own files (a bundled UE4SS, its settings, signatures,
  `mods.txt`, a `.usmap`), with a caution: they change UE4SS for every mod, so install them by hand, if at all.

It refuses the whole zip for any of these:
- a path that isn't plain: `..`, a drive, a device name such as `NUL`, or a stream (`:`);
- a link, or an encrypted file;
- a program or script file (`.exe`, `.bat`, `.ps1` and so on), or an archive inside it;
- a DLL other than a UE4SS mod's `dlls/main.dll`. Anything else would be loaded some other way, such as beside the
  game's executable;
- a file that would go where one already is in the game folder. It never unzips over anything;
- a name of UE4SS's own mods, or of this tool's merged containers;
- more than 5,000 files or 1 GB, or a file that expands too much for its size.

A container that only replaces game DataTables is turned into table edits, so its changes merge with other mods'
instead of replacing whole tables. The edits must give back exactly the mod's tables, or the container is kept as it
is. What can't be checked is recorded as a caution, shown in the mod's details and warned about whenever it's
installed:
- a C++ mod's detours;
- a kept container's whole replacements;
- Lua that opens files, runs programs or uses the network;
- a DLL that names networking, process or registry functions;
- table changes worked out against the current game build.

A caution isn't a verdict, and no check makes a mod safe: its Lua and DLLs run with your account's rights.

## After a game update

1. Start the game with UE4SS in a new save, and press **Ctrl + Numpad 6** (mappings) and
   **Ctrl + Numpad 9** (C++ headers).
2. Run:
   ```
   alchemists-tools update
   ```

It:
1. checks that both dumps are newer than the game;
2. re-extracts the tables, and reports which rows changed;
3. rebuilds the stub project's game module if the header dump changed;
4. compares the game Blueprints that mods copy with the last snapshot;
5. rebuilds every mod, and re-merges the installed ones against the new tables.

What it saw is kept in `build/.update/`.
