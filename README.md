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

- **Several mods together:** the game reads each table whole from the last container that has it, so
  installing merges the table edits of every installed mod into one container, in dependency order.
  Dependencies are written as in Factorio's `info.json`: `"VaultCore >= 0.2.0"`, `"? Optional"`,
  `"(?) Hidden"`, `"+ Recommended"`, `"~ NoOrder"`, `"! Incompatible"`.
- **Safe installs:** each installed mod has a manifest of its files with their SHA-256 hashes, and no
  two mods may own a file. A file is only deleted or replaced when it's listed, inside the game folder,
  and unchanged since it was installed.

**[docs/MODDING.md](docs/MODDING.md) is the full guide:**
- edit files;
- new recipes, research and buildings;
- `mod.json`;
- what the build checks;
- installing and manifests;
- the template save;
- Lua mods;
- cooked content;
- sharing;
- game updates.

Other commands:

| Command | What |
|---|---|
| `build-tables <folder>` | One folder of edit files -> a data container |
| `stub` | Set up the stub Unreal project for cooked content, from a blank C++ project, and build it |
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
