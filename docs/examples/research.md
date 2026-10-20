# Worked example: research and upgrades

The mod `examples/ExampleResearch/` (in this repo) changes a research's cost and time, and what an upgrade does. A new
research that unlocks a new recipe is in `docs/examples/recipes.md`. One that unlocks a new building is in
`docs/examples/buildings.md`.

| Change | Status |
|---|---|
| Healing Potion research: 300 copper (was 570), half the time (45 points, was 90) | Confirmed in game: 7.5 s (was 15 s) |
| Factory Efficiency 1 gives +50 % production speed (was +25 %) | Confirmed in game: machines ran 1.5 times as fast. Heat use rose by 50 % too, not by the 25 % of `FactoryConsumeSpeed` |

## The three tables

- **`DT_SkillPoints`** is the research tree (the Z panel). Each row usually unlocks one recipe or
  building; the `Level1`–`Level10` rows raise the technology level.
- **`DT_UpgradePoints`** is the upgrade tree, paid for in upgrade points: every upgrade costs 1. It has
  the same fields, but each row unlocks one `DT_Improvements` row, of the same name.
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
- `UnlockResearchPoints` sets the research time, in proportion: in a test save research ran at 6 points
  a second, so 90 points took 15 s. Cost and tier don't change the time.

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

## 2. A stronger upgrade

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

In game, machines ran 1.5 times as fast, but their heat use rose by 50 % too, so the fuel per product
didn't change. Heat use seems to follow production speed, and what `FactoryConsumeSpeed` does is still
to be worked out.

## 3. Build, install, test

```
alchemists-tools mod build ExampleResearch --mods examples
alchemists-tools mod install ExampleResearch --mods examples
```

In a new save:
- check the Healing Potion research's cost and time in its tooltip;
- buy Factory Efficiency 1, and check Device Production Speed.

## A new research node

To add a node, copy an existing research with `_copy` and change what differs:

```json
"DT_SkillPoints": {
  "PlanksToLargeGear": {
    "_copy": "WoodGear",
    "SlotPosition": {"X": -1, "Y": 5},
    "Predecessors": ["WoodGear"],
    "UnlockItem": {"ConfigName": "PlanksToLargeGear", "ConfigType": "CraftingRecipes"}
  }
}
```

- **What it unlocks:** `UnlockItem` with `ConfigType`:
  - `CraftingRecipes` for a recipe;
  - `ConstructOptions` for a building.

  `ExtraUnlockConstructions` adds more buildings, and `LevelUnlockItems` makes items purchasable.
- **Where it goes:** `SlotPosition`, below every predecessor, best in the row just below. A link runs
  down the predecessor's column, then sideways in the row above the node, so a node in that column in
  between looks like where the link comes from. The build warns about both. Two researches can't share
  a place; the build stops if they do.
- **What it needs first:** `Predecessors`.
- **Cost and time** are copied from the `_copy` row unless you give them.

## Things to know

- **Finished research stays finished.** Its unlock is copied into the save when it completes, so changing
  what a finished research unlocks has no effect on that save. Test in a new save.
- **`Level1` is already done** in a new game, so changing what it unlocks does nothing.
- **A deprecated node can't be brought back**: setting `Deprecated` to `false` didn't make one appear.
- **Removing a research doesn't crash** a save that has it.
