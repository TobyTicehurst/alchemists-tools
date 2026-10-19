# Alchemist's Tools

Modding tools for Alchemy Factory: extract the game's data tables as JSON, change them with edit
files, package content made in Unreal, and build, install and share mods.

## Install

Install [Rust](https://rustup.rs/), then:

```sh
cargo install --locked --git https://github.com/TobyTicehurst/alchemists-tools alchemists-tools
```

On first run, the tool downloads `oo2core_9_win64.dll` (the Oodle decompressor the game's
files need) next to itself.

## Dump the game's mappings (once per game update)

The tool needs a `.usmap` file describing the game's data layouts, dumped from the running game:

1. Install [UE4SS](https://docs.ue4ss.com/) into `AlchemyFactory/Binaries/Win64/`.
2. Start a brand new save. Once it has loaded, press **Ctrl + Numpad 6**.

The file appears in `Binaries/Win64/ue4ss/`, where the tool finds it.

## Extract the game's data

```sh
alchemists-tools extract
```

This writes every data table and curve in the game to `data/` as JSON, in the game's own folder
layout. For example, `data/DataTables/DT_Enemies.json` holds the items and
`data/DataTables/DT_EnemyCrafting.json` the recipes:

```json
"PortalSigil": {
  "ID": 2001,
  "DisplayName": "Portal Sigil",
  ...
```

A `null` value means the game file doesn't store it, so the game uses its built-in default.

To print a single table, with text kept as translation keys instead of English:

```sh
alchemists-tools decode DT_Railroads > railroads.json
```

## Make mods

A mod is a folder in `mods/` (`alchemists-tools mod new MyMod` starts one, with an example edit
file, `edits/example.json`, that applies nothing until you remove the `_` from a table's name):

```
mods/MyMod/
  mod.json               name, title, version, dependencies, cooked content, craft types
  edits/*.json           changes to the game's tables
  edits/if-Other/        changes applied only when the mod Other is installed too
  save/DefaultGame.json  changes to the save new games start from (numbers and true/false)
  ue4ss/                 a UE4SS Lua mod (Scripts/main.lua)
  ue4ss-shared/MyModX/   Lua modules other mods `require`
```

An edit file is `{table: {row: fields}}`, with the tables, rows and fields as `extract` writes them:

```json
{
  "DT_EnemyCrafting": {
    "WoodGear": {"ProductInfo": {"Count": 4}},
    "PlankNails": {
      "IngredientList": [{"IngredientName": "WoodBoard", "Count": 1}],
      "ProductInfo": {"IngredientName": "Nails", "Count": 5},
      "CraftType": "Process", "CraftingTime": 2.0
    }
  },
  "DT_Buildings": {"MyMachine": {"_copy": "Processor", "AlterBuildOption": "None"}}
}
```

Objects merge field by field and lists are replaced whole. `_copy` starts a new row as a copy of
another; a recipe the table doesn't have is new. Every value is checked against the game's layouts,
and likely mistakes are warned about (a machine without the belt or pipe connections a recipe needs,
a research on its predecessor's row, a new building nothing unlocks).

```sh
alchemists-tools mod build MyMod                       # -> build/MyMod/, laid out like the game folder
alchemists-tools mod install MyMod --with-dependencies
alchemists-tools mod uninstall MyMod [--force]
alchemists-tools mod dist MyMod                        # -> build/MyMod_<version>.zip, with a README
alchemists-tools mod list
alchemists-tools mod scan                              # every mod in the game folder, and its files
```

The game reads each table whole from the last container that has it, so installing merges the
table edits of every installed mod into one container, `zzz_ModTables_P`, in dependency order.
Dependencies are written as in Factorio's `info.json`: `"VaultCore >= 0.2.0"`, `"? Optional"`,
`"(?) Hidden"`, `"+ Recommended"`, `"~ NoOrder"`, `"! Incompatible"`.

**Manifests: what each mod owns.** Each installed mod has a manifest,
`Paks/~mods/manifests/<name>.json`: its name, version and dependencies, and every file it put in
the game folder, with its size and SHA-256 hash. `Paks/~mods/installed-mods.json` lists the
installed mods, the load order, and the hashes of the two files the tool merges for all of them
(`zzz_ModTables_P` and `zzz_ModSave_P`). No two mods may own the same file, so `install` stops
when a file it would write:
- is in another mod's manifest, or is one of the merged files;
- already exists and isn't this mod's (a mod installed by hand, say), or has changed since this
  mod installed it.

Nothing is deleted or replaced unless all of this holds:
1. it's in that mod's manifest (or, for a merged file, recorded as written by the tool);
2. its path is relative, has no `..`, starts with `AlchemyFactory/`, and resolves inside the game
   folder, links followed;
3. its size and hash are still those installed.

A file that has changed is kept, and `uninstall` lists it; `--force` deletes it too. A merged file
that has changed stops the install or uninstall before anything is touched. Folders are never
deleted with their contents: only files one by one, then the folders that leaves empty below
`ue4ss/Mods/`, so your own files in a mod's folder keep it.

`uninstall` also needs `--force`:
- when an installed mod requires the one being removed;
- for a mod without a manifest (installed by an older version, or by hand). It lists the files it
  would delete first; reinstalling the mod with this tool gives it a manifest.

A zip from `mod dist` carries the mod's manifest. So a mod a player extracts by hand is known too:
`mod scan` adds it to `installed-mods.json`, and `uninstall` removes it.

`mod scan` reports:
- every mod with a manifest, and whether its files are as installed;
- mods recorded without a manifest;
- the merged files;
- everything in `Paks/~mods` and `ue4ss/Mods` that no mod accounts for, with the game assets each
  unknown container replaces;
- assets that more than one container replaces (only one copy takes effect).

Other commands:

| Command | What |
|---|---|
| `build-tables <folder>` | One folder of edit files -> a data container |
| `package-content /Game/Mods/MyMod` | Content cooked in a stub Unreal project -> a content container |
| `probe <Name> <Class>:<Function>...` | A UE4SS mod that logs calls to game functions |
| `update` | After a game update: check the dumps, re-extract, rebuild the stub project, rebuild the mods |

## Options

| Option           | Use                                                        |
|------------------|------------------------------------------------------------|
| `--game <dir>`   | The game folder, if it isn't found in your Steam libraries |
| `--usmap <file>` | A `.usmap` other than the newest in the `ue4ss` folder     |
| `--out <dir>`    | Where `extract` writes (default `data`)                    |

Each command has `--help`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under MIT, see [LICENSE](LICENSE).
