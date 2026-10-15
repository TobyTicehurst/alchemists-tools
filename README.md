# Alchemist's Tools

Modding tools for Alchemy Factory: extract the game's data tables as JSON.

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

## Options

| Option           | Use                                                        |
|------------------|------------------------------------------------------------|
| `--game <dir>`   | The game folder, if it isn't found in your Steam libraries |
| `--usmap <file>` | A `.usmap` other than the newest in the `ue4ss` folder     |
| `--out <dir>`    | Where `extract` writes (default `data`)                    |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under MIT, see [LICENSE](LICENSE).
