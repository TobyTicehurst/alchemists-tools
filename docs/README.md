# Modding Alchemy Factory

What can be changed, how, and where each part is explained. Start with [MODDING.md](MODDING.md) for how mods are
made: edit files, mod folders, installing. Then come here for the details of each kind of change.

## What can be modded

| You want to change | How | Status | Guide |
|---|---|---|---|
| Recipes: ingredients, products, times | Table edit (`DT_EnemyCrafting`) | Confirmed | [recipes](examples/recipes.md) |
| New recipes for existing machines | Table edit, plus a research | Confirmed | [recipes](examples/recipes.md) |
| Research: costs, times, place in the tree, new nodes | Table edit (`DT_SkillPoints`) | New nodes confirmed; costs and times not tried | [research](examples/research.md) |
| Upgrades and their effects | Table edit (`DT_UpgradePoints`, `DT_Improvements`) | Not tried | [research](examples/research.md) |
| Buildings: cost, heat use, name, description | Table edit (`DT_Buildings`) | Name confirmed | [buildings](examples/buildings.md) |
| New buildings that reuse a game model | Table edit (`_copy`), plus a research | Confirmed | [buildings](examples/buildings.md) |
| Items: stack size, units, fuel, prices | Table edit (`DT_Enemies`) | Units confirmed | [items and economy](examples/items-and-economy.md) |
| What cauldrons make | Table edit (`DT_Enemies`: the cauldron values) | Not tried | [game mechanics](GAME-MECHANICS.md#cauldrons) |
| Quests (all but the first), land, contracts, plants, loans, customers, equipment, recognitions | Table edit | Quests and plants confirmed | [items and economy](examples/items-and-economy.md), [fields](FIELDS.md) |
| The start of a new game: first quest, starting values | Template-save edit (`save/DefaultGame.json`) | First quest confirmed | [saves](examples/saves.md) |
| Behaviour while the game runs | Lua script (UE4SS) | Confirmed | [Lua](examples/lua.md) |
| A machine of its own, with its own recipe list | Blueprint in the stub project, plus table edits | Confirmed | [Blueprints](examples/blueprints.md) |
| Icons and models of your own | Cooked content in the stub project | Not tried | [cooked content](examples/cooked-content.md) |
| A coin's worth, the 5,000-building deconstruct limit | Not possible yet: in the game's code | | [fields](FIELDS.md#what-isnt-in-any-table) |

*Confirmed*: seen working in game. *Not tried*: the mod builds and the tools check it, but nobody has
checked the result in game yet.

## The guides

- **[FIELDS.md](FIELDS.md)**: every data table and field, and what is known about each.
- **[STUB-PROJECT.md](STUB-PROJECT.md)**: setting up and using the stub Unreal project, for Blueprints and
  other cooked content.
- **[GAME-MECHANICS.md](GAME-MECHANICS.md)**: how daily quests, customers and machines use the tables.
- **[MACHINE-ART.md](MACHINE-ART.md)**: making a machine's model and icon, in Blender, Krita and the
  editor.
- **Worked examples**, each built on a mod in the repo's `examples/` folder. Each one builds and installs
  alongside the others:

  | Guide | Mod |
  |---|---|
  | [Recipes](examples/recipes.md) | `ExampleRecipes` |
  | [Research and upgrades](examples/research.md) | `ExampleResearch` |
  | [Buildings](examples/buildings.md) | `ExampleBuildings` |
  | [Items, quests and the economy](examples/items-and-economy.md) | `ExampleEconomy` |
  | [The template save](examples/saves.md) | `QuickStart` |
  | [Lua mods](examples/lua.md) | `ExampleBagReporter`, `AlchemistsLibrary`, `VaultProbe` |
  | [A machine with its own Blueprint](examples/blueprints.md) | `MyMachine` |
  | [Models, textures and icons](examples/cooked-content.md) | none yet |

Elsewhere:
- the repo's `README.md`: installing the tool, and its commands (each also has `--help`).

## Trying an example

The examples are mod folders in `examples/`. Copy one into your own `mods/` folder, or run the commands
from the repo with `--mods examples`:

```
alchemists-tools extract                                        # the game's tables, as JSON in data/
alchemists-tools mod build ExampleRecipes --mods examples        # check it, and see what it changes
alchemists-tools mod install ExampleRecipes --mods examples      # then restart the game
alchemists-tools mod uninstall ExampleRecipes --mods examples
```

Try mods in a new save, or a copy of one. Saves are in
`%LOCALAPPDATA%/AlchemyFactory/Saved/SaveGames/`. **A mod that adds a building must stay installed in
every save with one placed**: the save crashes on load without it. Deconstruct those buildings before
uninstalling.

When you try something marked *not tried*, change its status here, in the guide and in `FIELDS.md`.
