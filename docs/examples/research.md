# Worked example: research and upgrades

The mod `examples/ExampleResearch/` (in this repo) changes a research, an upgrade's cost, and what that upgrade does. A new
research that unlocks a new recipe is in `docs/examples/recipes.md`. One that unlocks a new building is in
`docs/examples/buildings.md`.

| Change | Status |
|---|---|
| Healing Potion research: 300 copper (was 570), 45 s (was 90 s) | Builds; not tried in game |
| Factory Efficiency 1 costs 30 upgrade points (was 60) | Builds; not tried in game |
| Factory Efficiency 1 gives +50 % production speed (was +25 %) | Builds; not tried in game |

## The three tables

- **`DT_SkillPoints`** is the research tree (the Z panel). Each row usually unlocks one recipe or
  building; the `Level1`–`Level10` rows raise the technology level.
- **`DT_UpgradePoints`** is the upgrade tree, paid for in upgrade points. It has the same fields, but each
  row unlocks one `DT_Improvements` row, of the same name.
- **`DT_Improvements`** says what an upgrade does: a list of changes to attributes (`DT_Attributes`).
  Equipment and recognitions use it too.

## 1. A cheaper, faster research

Find the research in `data/DataTables/DT_SkillPoints.json`:

```json
"HealingPotion": {
  "Tier": 2,
  "SlotPosition": {"X": 0, "Y": 8},
  "Predecessors": ["Assembler"],
  "UnlockCost": {"X": 0, "Y": 0, "Z": 570},
  "UnlockResearchPoints": 90.0,
  "UnlockItem": {"ConfigName": "HealingPotion", "ConfigType": "CraftingRecipes"},
  ...
```

- `UnlockCost` is a price: X gold, Y silver, Z copper.
- `UnlockResearchPoints` is the research time in seconds, at 100 % Research Speed.

```json
{
  "DT_SkillPoints": {
    "HealingPotion": {
      "UnlockCost": {"Z": 300},
      "UnlockResearchPoints": 45.0
    }
  }
}
```

`UnlockCost` is an object, so giving only `Z` keeps `X` and `Y` at 0.

## 2. A cheaper upgrade

In `DT_UpgradePoints`, `UnlockResearchPoints` is the cost in upgrade points: 60 × tier², so tier 1 costs
60. `UnlockCost` isn't a cost here: its `Z` just counts rows. Leave it alone.

```json
{
  "DT_UpgradePoints": {
    "FactorySpeed1": {"UnlockResearchPoints": 30.0}
  }
}
```

## 3. A stronger upgrade

The upgrade `FactorySpeed1` unlocks the improvement `FactorySpeed1`:

```json
"FactorySpeed1": {
  "DisplayName": "Factory Efficiency",
  "Effects": [
    {"AttributeName": "FactorySpeed", "ModificationType": "Increase", "ModValue": 25.0},
    {"AttributeName": "FactoryConsumeSpeed", "ModificationType": "Increase", "ModValue": 25.0}
  ],
  ...
```

`Increase` adds percentage points to a percentage attribute; `Add` adds a flat amount (`Bag1` adds 4
inventory slots). `Effects` is a list, so the edit gives both entries:

```json
{
  "DT_Improvements": {
    "FactorySpeed1": {
      "Effects": [
        {"AttributeName": "FactorySpeed", "ModificationType": "Increase", "ModValue": 50.0},
        {"AttributeName": "FactoryConsumeSpeed", "ModificationType": "Increase", "ModValue": 25.0}
      ]
    }
  }
}
```

The attributes, with their base values, are listed in `docs/FIELDS.md`.

## 4. Build, install, test

```
alchemists-tools mod build ExampleResearch --mods examples
alchemists-tools mod install ExampleResearch --mods examples
```

In a new save:
- check the Healing Potion research's cost and time in its tooltip;
- check Factory Efficiency 1's cost, and Device Production Speed once it's bought.

## A new research node

To add a node, copy an existing research with `_copy` and change what differs:

```json
"DT_SkillPoints": {
  "PlanksToLargeGear": {
    "_copy": "WoodGear",
    "SlotPosition": {"X": -2, "Y": 5},
    "Predecessors": ["Processor"],
    "UnlockItem": {"ConfigName": "PlanksToLargeGear", "ConfigType": "CraftingRecipes"}
  }
}
```

- **What it unlocks:** `UnlockItem` with `ConfigType`:
  - `CraftingRecipes` for a recipe;
  - `ConstructOptions` for a building.

  `ExtraUnlockConstructions` adds more buildings, and `LevelUnlockItems` makes items purchasable.
- **Where it goes:** `SlotPosition`, at least one row below every predecessor. Links are drawn downwards,
  so a node level with its predecessor gets a link through that node. Two researches can't share a
  place; the build stops if they do.
- **What it needs first:** `Predecessors`.
- **Cost and time** are copied from the `_copy` row unless you give them.

## Things to know

- **Finished research stays finished.** Its unlock is copied into the save when it completes, so changing
  what a finished research unlocks has no effect on that save. Test in a new save.
- **`Level1` is already done** in a new game, so changing what it unlocks does nothing.
- **A deprecated node can't be brought back**: setting `Deprecated` to `false` didn't make one appear.
- **Removing a research doesn't crash** a save that has it.
