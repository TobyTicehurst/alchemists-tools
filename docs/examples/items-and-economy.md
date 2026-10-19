# Worked example: items, quests and the economy

The mod `examples/ExampleEconomy/` (in this repo) changes items, a plant, a quest, land, and a purchase contract.

| Change | Status |
|---|---|
| Limestone holds 300 units (was 150) | Confirmed in game: 300 Stone per Limestone |
| Flax needs 12 V of fertilizer (was 24) | Confirmed in game |
| The second main quest asks for 25 Linen Rope (was 50) | Confirmed in game |
| Coal gives 700 heat (was 540) | Builds; not tried in game |
| Healing Potions sell for 120 copper (was 85) | Builds; not tried in game |
| Land 1 costs 3,000 copper (was 6,000), and 100 a day (was 150) | Builds; not tried in game |
| Up to 32 Bandage contracts a day (was 16) | Builds; not tried in game |

## Items

Items are rows of `DT_Enemies`. The game's code calls items "enemies": it began as a tower-defence
prototype. From `data/DataTables/DT_Enemies.json`:

```json
"Limestone": {
  "ID": 2,
  "DisplayName": "Limestone",
  "MaximumStack": -150,
  "StockCost": {"X": 0, "Y": 0, "Z": 600},
  ...
```

`edits/items.json`:

```json
{
  "DT_Enemies": {
    "Limestone": {"MaximumStack": -300},
    "Coal": {"HeatValue": 700.0},
    "HealingPotion": {"CostValue": {"Z": 120}}
  },
  "DT_PlantSeedConfig": {
    "FlaxSeed": {"GrowthNutrientValue": 12.0}
  }
}
```

- **`MaximumStack`**: positive, it's the stack size. Negative, the item is a *unit item* holding that
  many units: the Stone Crusher makes one Stone per unit of Limestone.
- **`HeatValue`**: heat per unit when burnt as fuel.
- **`CostValue`**: the base price in your shop. **`StockCost`** is what it costs to buy from the merchant
  or the portal. Both are prices: X gold, Y silver, Z copper.
- **`GrowthNutrientValue`** (`DT_PlantSeedConfig`, by seed): the fertilizer a plant needs, the "V" in
  game. With the edit made while the game ran, the Flax tooltip still said 24 V while growth took 12. A
  `.pak` mod loads before the game reads the table, so its tooltip may well be right, but that hasn't
  been checked.

The other item fields (quest and shop tiers, how much customers buy, models, icons) are in
`docs/FIELDS.md`.

## Quests

`DT_Quests` holds the main quests (`Main1`–`Main9`), the Carpenter and Sample Requests, and the templates
for daily quests. `edits/economy.json` changes the second main quest:

```json
{
  "DT_Quests": {
    "Main2": {
      "RequireProducts": {"Nums": [25]}
    }
  }
}
```

`RequireProducts` pairs a list of items (`Items`) with a list of counts (`Nums`). Only `Nums` is given, so
the item stays Linen Rope. Rewards are in `Awards`: `Gold` (a price), `Products` (items, with unit items
in units), `Buildings` and `Contracts` (unlocked).

**The first quest, `Main1`, can't be changed here.** A new game starts from a template save that already
holds it. QuickStart changes it with a save edit (`docs/examples/saves.md`). Quests already given keep
their own copy, so a change reaches only the quests given after it.

## Land and purchase contracts

```json
{
  "DT_GridZones": {
    "Land1": {"UnlockCost": {"Z": 3000}, "DailyRent": 100}
  },
  "DT_SupplyContract": {
    "Bandage": {"DailyLimit": 32}
  }
}
```

- **`DT_GridZones`**: each plot's price (`UnlockCost`, a price) and rent per day (`DailyRent`, copper).
- **`DT_SupplyContract`**: one row per contract item, with how many items a contract takes (`SellItem`),
  the pay per contract (`ContractReward`), and contracts per day (`DailyLimit`).

## Build, install, test

```
alchemists-tools mod build ExampleEconomy --mods examples
alchemists-tools mod install ExampleEconomy --mods examples
```

In a new save:
- break a Limestone in a Stone Crusher and count the Stone;
- check Coal's heat value and the Healing Potion's price in their tooltips;
- check Land 1's price;
- check that the second main quest asks for 25 Linen Rope.

## Things to know

- **Coins can't be changed.** Copper, silver and gold coins are items, but their worth is fixed in the
  game's code.
- **Loans:** the template save holds its own four loans, with different amounts from `DT_Loans`. Which
  one a new game uses hasn't been checked.
- **Customers:** `DT_Customers` sets who comes and which shop categories they buy from. How many items
  they pick comes from two curves (`CV_NormalPurchase`, `CV_MerchantPurchase`), which edit files can't
  change yet. `docs/GAME-MECHANICS.md`, "Customers", has the formulas.
- **Attributes** (`DT_Attributes`: base production speed, belt speed, inventory size, …): whether a
  changed base value reaches a game is unknown. Changing what an upgrade gives
  (`docs/examples/research.md`) is another way, also not tried yet.
