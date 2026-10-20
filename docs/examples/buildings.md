# Worked example: buildings

The mod `examples/ExampleBuildings/` (in this repo) makes the Kiln cheaper to build and adds a new building: the Stone Processor, a
Processor built from Stone, with a research that unlocks it.

| Change | Status |
|---|---|
| Kiln: 10 Stone + 5 Clay (was 20 + 10) | Confirmed in game |
| New building: Stone Processor, with its research | Builds. The same kind of row (a copied Processor) worked in game |

A building's data is a row of `DT_Buildings`. Its model, parts and behaviour are its Blueprint, which the
row names in `ScriptClass`. With table edits alone you can change a building's cost, heat use, name,
description, icon and ports, and add new buildings that reuse an existing Blueprint. A model or recipe
list of its own needs a Blueprint of its own: `docs/examples/blueprints.md`.

## 1. Change a building

From `data/DataTables/DT_Buildings.json`:

```json
"Kiln": {
  "ID": 203,
  "HeatCost": 15.0,
  "CostList": [
    {"IngredientName": "Stone", "Count": 20},
    {"IngredientName": "Clay", "Count": 10}
  ],
  ...
```

```json
{
  "DT_Buildings": {
    "Kiln": {
      "CostList": [
        {"IngredientName": "Stone", "Count": 10},
        {"IngredientName": "Clay", "Count": 5}
      ]
    }
  }
}
```

`CostList` is a list, so every ingredient is given.

**`HeatCost` can't be changed this way yet.** It's the heat used per second, and the tooltip follows it,
but a Kiln built with it set to 10 still used 15 a second. Where the real value comes from is still to
be worked out.

## 2. Add a building

`_copy` starts a new row as a copy of an existing one. The copy keeps the original's Blueprint, so it has
the same model, size, ports and recipes:

```json
{
  "DT_Buildings": {
    "StoneProcessor": {
      "_copy": "Processor",
      "ID": 221,
      "AlterBuildOption": "None",
      "CostList": [
        {"IngredientName": "Stone", "Count": 20},
        {"IngredientName": "LargeWoodGear", "Count": 3}
      ],
      "DisplayName": "Stone Processor",
      "DisplayDesc": "A Processor built from Stone instead of Planks. Processes materials into components."
    }
  }
}
```

- **`ID`** places it in the build menu, which sorts by ID within groups of a hundred. 2xx is active
  crafting machines; the Processor is 201. Without an `ID`, a copy gets the first free one in its
  original's hundred. Here it's given, because `examples/MyMachine` already uses 220 and a mod installed with
  it would otherwise get the same number. The build stops when two rows share an ID.
- **`AlterBuildOption`** is the building's mirrored form (the Processor's is `Processor_Sym`, a separate
  row with its own Blueprint). Set it to `"None"` on a copy, or the copy's mirror would be the
  Processor's.
- **`DisplayName` and `DisplayDesc`** are texts. A text you give is the same in every language; one you
  leave alone keeps the copied row's translations.

Every new building needs a research that unlocks it, with `ConfigType` `ConstructOptions`:

```json
{
  "DT_SkillPoints": {
    "StoneProcessor": {
      "_copy": "Processor",
      "SlotPosition": {"X": 2, "Y": 4},
      "Predecessors": ["Processor"],
      "UnlockItem": {"ConfigName": "StoneProcessor", "ConfigType": "ConstructOptions"}
    }
  }
}
```

The node goes in the row just below its predecessor, in a free place (`docs/examples/research.md`).

## 3. Build, install, test

```
alchemists-tools mod build ExampleBuildings --mods examples
alchemists-tools mod install ExampleBuildings --mods examples
```

The install names the new building with a warning (below). In a new or Sandbox save:
1. Research the Stone Processor: one row below the Processor's research, two columns right.
2. Find it last among the active crafting machines in the build menu, and check its cost and name.
3. Place it, and check that it crafts like a Processor.

## Never remove a building that has been placed

**A save with a placed building whose row is missing crashes when it loads.** So:
- before uninstalling, deconstruct every placed Stone Processor in every save you keep;
- never rename or remove a building row in a mod people use.

The install, the uninstall and the README `dist` writes all say this for each new building.

## Other things to change

| To change | Field |
|---|---|
| The build-menu icon | `DisplayIcon`: a game texture's path, or your own (`docs/examples/cooked-content.md`) |
| Its connections | `InOutList`: the belt and pipe ports, one entry per connection (`docs/FIELDS.md`) |
| Its footprint | `GridConfigList`, `VolumeList`. They must match the model, so copy them from a building of the same size |
| Hidden or shown | `bHideInGame`, `bHideInCreative` |
| What a machine crafts | not a table field: the Blueprint's craft type (`docs/examples/blueprints.md`) |
| How fast a machine works | not a table field. Machines have no speed of their own; only the global `FactorySpeed` attribute and recipe times |
| How much a chest holds | not a table field: the Blueprint's container capacity |
