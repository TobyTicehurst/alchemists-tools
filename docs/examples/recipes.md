# Worked example: recipes

The mod `examples/ExampleRecipes/` (in this repo) changes four recipes, and adds a new Processor recipe with the research that
unlocks it. This page builds it step by step.

| Change | Status |
|---|---|
| Processor: 1 Large Wooden Gear → 4 Small Wooden Gears (was 3) | Confirmed in game |
| Extractor: 1 Flax → 30 Linseed Oil (was 20) | Confirmed in game |
| Brew Barrel: 20 Linseed Oil + 80 Fruit Wine (was 40 + 80) | Confirmed in game |
| Kiln: 3 Sand → 1 Glass in 4 s (was 6 Sand, 6 s) | Builds; not tried in game |
| New: Processor, 2 Planks → 1 Large Wooden Gear in 8 s, unlocked by a new research | Builds; a new recipe and research worked in an earlier test |

## 1. Find the recipe

Recipes are rows of `DT_EnemyCrafting`. Extract the game's tables once (and again after each game update):

```
alchemists-tools extract
```

Open `data/DataTables/DT_EnemyCrafting.json` and search for the product. The Small Wooden Gear's recipe is
the row `WoodGear`:

```json
"WoodGear": {
  "ID": 207,
  "IngredientList": [{"IngredientName": "LargeWoodGear", "Count": 1}],
  "ProductInfo": {"IngredientName": "WoodGear", "Count": 3},
  "CraftType": "Process",
  "CraftingTime": 12.0,
  "FractionNum": 1,
  ...
```

Items are named by their IDs, not their English names. `DT_Enemies.json` maps one to the other: its row
`WoodGear` has the `DisplayName` "Small Wooden Gear". What each field means is in `docs/FIELDS.md`.

## 2. Start the mod

```
alchemists-tools mod new ExampleRecipes
```

This makes `mods/ExampleRecipes/` with a `mod.json` and an example edit file. (The finished mod is in
`examples/ExampleRecipes/`: build it with `--mods examples`, or copy it into `mods/`.) Put your own edit files in
its `edits/` folder. Any name ending in `.json` works; the files are read in alphabetical order.

## 3. Change existing recipes

`edits/changes.json`:

```json
{
  "DT_EnemyCrafting": {
    "WoodGear": {
      "ProductInfo": {"Count": 4}
    },
    "LinseedOil": {
      "ProductInfo": {"Count": 30}
    },
    "WhisperingFields": {
      "IngredientList": [
        {"IngredientName": "LinseedOil", "Count": 20},
        {"IngredientName": "FruitWine", "Count": 80}
      ]
    },
    "Glass": {
      "IngredientList": [{"IngredientName": "Sand", "Count": 3}],
      "CraftingTime": 4.0
    }
  }
}
```

- **Give only what changes.** `ProductInfo` is an object, so `{"Count": 4}` keeps its `IngredientName`.
- **Lists are replaced whole.** `IngredientList` is a list, so the Brew Barrel's entry lists both
  ingredients, even though only one count changes. Leaving one out would remove it from the recipe.
- **Times are seconds per craft** at 100 % production speed.

The real file has a `_comment` on each row. Any key starting with `_` is a comment.

## 4. Add a recipe

A row name the table doesn't have makes a new recipe. It needs at least `IngredientList`, `ProductInfo`,
`CraftType` and `CraftingTime`. `CraftType` decides which machines run it (the list is in
`docs/FIELDS.md`); `Process` is the Processor.

A new recipe can't be used until a research unlocks it. So `edits/new-recipe.json` adds both:

```json
{
  "DT_EnemyCrafting": {
    "PlanksToLargeGear": {
      "IngredientList": [{"IngredientName": "WoodBoard", "Count": 2}],
      "ProductInfo": {"IngredientName": "LargeWoodGear", "Count": 1},
      "CraftType": "Process",
      "CraftingTime": 8.0
    }
  },
  "DT_SkillPoints": {
    "PlanksToLargeGear": {
      "_copy": "WoodGear",
      "SlotPosition": {"X": -2, "Y": 5},
      "Predecessors": ["Processor"],
      "UnlockItem": {"ConfigName": "PlanksToLargeGear", "ConfigType": "CraftingRecipes"}
    }
  }
}
```

The recipe:
- gets a free `ID`;
- isn't hidden;
- is marked as an alternative (`bAlternate`), since the Grinder already makes Large Wooden Gears.

The research:
- **`_copy`** starts it as a copy of the Small Wooden Gear's research, with the same tier, cost and time.
- **`SlotPosition`** places it in the tree: X is left and right, Y is down. It must be at least one row
  below its predecessors, because links are drawn downwards. The Processor's research is at (0, 3). The
  build stops if two researches share a place, so check `DT_SkillPoints.json` for free ones.
- **`UnlockItem`** names the recipe, as a `CraftingRecipes` unlock.

The details are in `docs/examples/research.md`.

## 5. Build, install, test

```
alchemists-tools mod build ExampleRecipes --mods examples
```

The build checks every name and value, and lists each change:

```
.pak mod: 6 row(s) in 2 table(s), checked
   DT_EnemyCrafting.WoodGear.ProductInfo.Count = 4
   ...
   new row DT_EnemyCrafting.PlanksToLargeGear (starting from Mortar)
   new row DT_SkillPoints.PlanksToLargeGear (starting from WoodGear)
```

It also warns about likely mistakes, such as:
- a recipe whose item needs a belt or pipe connection the machine doesn't have;
- a side product on a machine that discards side products.

Then install it, and restart the game:

```
alchemists-tools mod install ExampleRecipes --mods examples
```

In a new or Sandbox save:
1. Research the new node, two columns left of the Processor's and two rows below it.
2. Choose the new recipe on a Processor, and check that 2 Planks make 1 Large Wooden Gear.
3. Check that a Processor making Small Wooden Gears makes 4 per Large Wooden Gear.

To remove the mod: `alchemists-tools mod uninstall ExampleRecipes --mods examples`.

## Things to know

- **Machines differ** (`docs/FIELDS.md`, "How machines read a recipe"):
  - active machines (Processor, Assembler, Kiln, …) follow every field;
  - the Grinder makes one item per craft, whatever the count;
  - raw-production machines (Table Saw, Stone Crusher, Smelter) speed up with a larger count, but the
    total per input item comes from the input's `MaximumStack`.
- **Unit items** (negative `MaximumStack`: Logs, Limestone, Iron Ore, …) are counted in units. The Table
  Saw's recipe is 1 unit of Logs → 1 Plank, with `FractionNum` 200: one Log is 200 crafts.
- **Side products** (`SideProduct`) work only on raw-production machines. **Fail products** are for the
  Athanors.
- **Don't rename a recipe that's in use.** Saves remember a machine's recipe by its row name. A missing
  recipe doesn't crash, but the machine's recipe becomes unset.
- **More ingredients than ports is fine.** The number of belt inputs doesn't limit the number of
  ingredients (tested: three ingredients on the two-input Assembler).
