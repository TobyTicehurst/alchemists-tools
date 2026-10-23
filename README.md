# Alchemist's Tools

Modding tools for Alchemy Factory: extract the game's data tables as JSON, change them with edit
files, package content made in Unreal, and build, install and share mods.

## Install

Download `alchemists-tools-setup.exe` from the [latest release](https://github.com/TobyTicehurst/alchemists-tools/releases/latest)
and run it. It downloads that release's files and checks them against the checksum built into it, then:
- puts the GUI, the command line and the game's mappings in `%LOCALAPPDATA%\Programs\AlchemistsTools`;
- adds that folder to your PATH, so `alchemists-tools` works in any new terminal;
- adds Alchemist's Tools to the Start menu, and to Windows' Installed apps, where it uninstalls.

It then offers to install UE4SS into the game, which mods with Lua or C++ need (the build they're made for, checked
against its checksum; Setup in the GUI does the same later). It needs no administrator rights. Run a newer release's
installer to update; uninstalling keeps your mods folder, settings and UE4SS. On first use, the tools download `oo2core_9_win64.dll` (the Oodle decompressor the game's files need)
next to themselves.

To build from source instead, install [Rust](https://rustup.rs/), then:

```sh
cargo install --locked --git https://github.com/TobyTicehurst/alchemists-tools alchemists-tools
cargo install --locked --git https://github.com/TobyTicehurst/alchemists-tools alchemists-gui   # the desktop interface
```

Installed that way, the tools don't have the game's mappings: dump your own (below), or point the settings at the
repository's `mappings/<build>.usmap`.

## The game's mappings

The tools read the game's tables through its mappings, a `.usmap` file of the game's data layouts. The installer puts
the mappings for the game's current Steam build beside the tools (`mappings/`), so players needn't make their own. After a game
update, a new release brings the new build's. Until then, or to make mods, dump your own from the running game:

1. Install [UE4SS](https://docs.ue4ss.com/) into `AlchemyFactory/Binaries/Win64/`.
2. Start a brand new save. Once it has loaded, press **Ctrl + Numpad 6**.

The file appears in `Binaries/Win64/ue4ss/`, where the tool finds it. The tools' own are used first when they're for
the game's build. If UE4SS came with AF Mod Loader, the key
does nothing at first: see [docs/examples/lua.md](docs/examples/lua.md), "Setting up UE4SS".

## Install mods with the GUI

`alchemists-gui` lists the mods in your mods folders, like Factorio's mod list. Tick the mods you want installed
and click **Confirm**: it builds every ticked mod, checks them together, installs them, and uninstalls the rest.
It happens entirely or not at all. If a build fails, two mods clash (they add the same row, or detour the same game
function), a required mod isn't ticked, or a file can't be replaced (the game is running), the game folder is left as
it was. The reason is shown, and the mods it's about are marked ✖. After a successful install, mods that warned are
marked ⚠.

Clicking a mod shows its details: its picture (`thumbnail.png` in its folder), version, dependencies, the game build
it's made for (marked when the game is newer), the tables it changes and the game functions it hooks. Ticking a mod
ticks the mods it requires; unticking one unticks the mods that require it. Files installed by hand or by another
tool are listed apart, since this tool can't remove them. Mods installed from a zip, or without a manifest, are
marked, since they need care.

**The mods folder** holds every mod's source, installed or not: unticking a mod uninstalls it from the game, and it
stays in the list, unticked. A mod installed from somewhere else is kept there as a zip when it's unticked, so it can
be ticked again. Put a mod's zip there (a download from Nexus Mods or Thunderstore) to list it.

**Import a zip** takes a mod made without this tool, such as an older download from a mod site, as far as it can
safely: it refuses zips it can't place safely, and marks what it can't check (see
[docs/MODDING.md](docs/MODDING.md), "Mods made without this tool").

**Setup** shows what playing with mods needs: whether the game, UE4SS and the mappings are found, and where the mods
folders are. It installs UE4SS, and disables it after a game update it can't handle yet. (`alchemists-tools check` also shows what making mods needs.)
The settings are shared with the command line, and everything the GUI does is a command too:

| GUI | Command |
|---|---|
| The mod list | `alchemists-tools mod list` |
| Confirm | `alchemists-tools mod apply <names>...` (the mods to have installed) |
| Import a zip (a mod made without this tool) | `alchemists-tools mod import <zip>` |
| Setup | `alchemists-tools check`, `alchemists-tools settings` |
| Setup's UE4SS buttons | `alchemists-tools ue4ss install`, `remove`, `disable`, `enable` |

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
  ue4ss/                 a UE4SS mod: Lua (Scripts/main.lua) or C++ (dlls/main.dll)
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
alchemists-tools mod apply MyMod OtherMod             # exactly these installed: all at once, or nothing changes
alchemists-tools mod dist MyMod                        # -> build/MyMod_<version>.zip: for Nexus, Thunderstore, or by hand
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
| `check` | Whether the game, UE4SS, the `.usmap` and the other things the tools use are found |
| `settings` | Show or save where the game, mods folders and builds are (shared with the GUI) |
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
| `--mods <dir>`   | A folder of mod folders (default `mods`); repeat for several |
| `--out <dir>`    | Where `extract` writes (default `data`)                    |

Options not given come from the saved settings: `alchemists-tools settings --mods <dir> --mods <dir>` saves them once.

Each command has `--help`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under MIT, see [LICENSE](LICENSE).
