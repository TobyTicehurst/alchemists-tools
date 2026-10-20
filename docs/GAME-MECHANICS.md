# How the game uses its data

How some of the game's systems turn the tables' values into what happens in game, for working out what a
change will do. It was worked out by reading the game's code, and checked against saves and play where
noted. Field names are as in `docs/FIELDS.md`.

- [Daily quests](#daily-quests)
- [Customers](#customers)
- [When an item counts as available](#when-an-item-counts-as-available)
- [Time and sleep](#time-and-sleep)
- [How machines read recipes](#how-machines-read-recipes)
- [Unit items and recipe cycles](#unit-items-and-recipe-cycles)

## Daily quests

New daily quests are made each day, from the three templates in `DT_Quests` (`RandomNormal`,
`RandomUrgent`, `RandomBulk`). The quest system has its own random stream, reseeded every day.

- **How many:** `round(ExtraQuest)` a day. The attribute `ExtraQuest` starts at 1, and upgrades
  `QuestSkill4`, `QuestSkill8` and `QuestSkill12` add 1 each: at most 4 a day.
- **Which template:** a weighted pick by `RandomWeight` (Normal 3, Urgent 1, Bulk 1), among those that
  can appear today:
  - `Conditions.DateRange` contains today;
  - `Conditions.GameLevelRange` contains the alchemy level;
  - every quest in `Conditions.RequireQuests` is done.

  Normal needs Main1, Urgent needs Main2, and Bulk needs Main2 and level 3. So it's all Normal until Main2,
  then 75 / 25 % Normal / Urgent, then 60 / 20 / 20 % from level 3. The alchemy level starts at 0, and
  each main quest adds 1. The research nodes `Level1`–`Level10` are a separate technology level.
- **How long it stays:** a quest ends when `today − accepted day > DateRange.Y`. So an Urgent quest
  (Y = 0) lasts 1 day, a Normal one (Y = 2) 3 days, and a Bulk one (Y = 4) 5 days.
- **How many items:** a whole number picked evenly between `len(RequireProducts.Items)` and
  `len(RequireProducts.Nums)`. That's Normal 2–3, Urgent 1 and Bulk 4–5. It's capped at the number of
  candidates.
- **Which items** can be asked for:
  - items not hidden (`bHideInGame` false);
  - with `RandomQuestBaseNum` above 0;
  - with a tier of at most `L`, where `L = max(2, level at the start of the day + 1)`. The tier is
    `QuestTier`, or `ID ÷ 100` (rounded down) when `QuestTier` is 0;
  - an item whose tier comes from its `ID` and equals `L` must also be available
    ([below](#when-an-item-counts-as-available)).

  There's no filter by shop category, so Worthless items can be asked for. Items are drawn without
  repeats, weighted by `0.4 × tier²`, so higher tiers are much more likely.
- **How much of each:** `RandomQuestBaseNum × m`, where m is 3 for one item, 1 for two or three, and 2 for
  four or more. Amounts are whole items, unit items included.
- **Reward:** the sum over the items of `round(amount × QuestCost × GoldFactor × QuestProfit ÷ 100)`
  copper. `GoldFactor` is Normal 1.3, Urgent 2.0 and Bulk 1.6. `QuestProfit` starts at 100 and grows with
  the QuestSkill upgrades. Fixed rewards (`Awards.Gold`, on the other quests) are scaled by
  `QuestProfit ÷ 100` too.

Checked against a long save (a level-4 game, 35 quests): 3 quests a day, the tier-5 items asked for were
only those the player could make, and the template split matched the weights.

## Customers

Customers only come while the shop is open. Times here are in real seconds.

- **Walk-ins:** every `60 − 0.2 × R` seconds, `floor(4 + 0.02 × R)` customers join a queue, cut to
  `10 −` the customers already in the shop. The queue lets one in every 3 seconds. R is the
  `StoreReputation` attribute.
- **Through a Grand Portal**, once one is built (building more doesn't add traffic): one customer every
  `hi − lo` to `hi + lo` seconds, with `lo = 15 − 0.05 × P`, `hi = 45 − 0.15 × P`, and P the
  `PortalTrafficRate` attribute. At P = 100 that's one every 20–40 s. Portal customers only come while
  fewer than 14 are about.
- **Idle visitors** wander about without buying. They're picked by `IdlePr`.
- **Which kind of customer** (`DT_Customers`): among those with `UnlockLevel` at most the alchemy level,
  weighted by `BasePr + R × RepPrMulti`. A kind with a weight of 0 or less doesn't come.
- **What they buy:**
  1. A number of picks n from 1 to 6, with chance in proportion to the purchase curve at n
     (`PurchaseNumberCurve`). That's 20/29/24/16/8/2 % for most customers (2.7 on average), and
     4/9/15/20/25/27 % for Merchants (4.3).
  2. In total, `round(n × (1 + CustomerBuyNum ÷ 100))` picks. `CustomerBuyNum` starts at 0 and grows with
     the Bargaining upgrades.
  3. For each pick:
     - choose a shop category by the customer's `GoodsPrMap` weights;
     - then choose an item of that `SellType` evenly at random. It must not be hidden, and its shop tier
       (`ShoppingTier`, or `ID ÷ 100`) must be at most `L`, as for quests (99 in Sandbox).

     A category with no such items wastes the pick.
  4. The amount of each item is a whole number from 1 to its `NpcShoppingBaseNum`. Picks of the same item
     add up.
- **Checkout:** each register serves one customer every `1 ÷ CheckoutSpeed` seconds.
- **Reputation** (`StoreReputation`) comes only from recognitions, 8 each, and never goes down.

A save that sold 1,000–1,470 items a day was predicted at 1,211 a day from these rules.

## When an item counts as available

Used for the daily-quest and shop tiers above. An item is available when any of these holds:
1. it can be bought (it's in the save's unlocked supply orders);
2. it comes from a building the player has. The game keeps a fixed list of these: Steam from the Steam
   Boiler; Mors and Vitae from the Paradox Crucible; Ruby, Sapphire, Emerald and the Philosopher's Stone
   from the Cauldron; World Tree Leaf and Core from the World Tree Nursery;
3. an unlocked recipe that isn't an alternative makes it, as its product or a fail product but not a side
   product:
   - for the passive and raw-production craft types (TableSaw, StoneCrusher, Smelter, Plant, Grind,
     Extract, Calcinate, Refine, Paradox, Cauldron), that means the type's machine is built and the
     recipe's first ingredient is available itself;
   - for the others, the recipe must be unlocked.

An item with an explicit `QuestTier` or `ShoppingTier` skips this check.

## Time and sleep

- One game hour is 60 game seconds; customer and quest timers count the same seconds. So one real minute
  is one game hour.
- A day lasts `24 − SleepTime` real minutes of play. `SleepTime` starts at 8 hours and each Potion Belt
  tier takes 2 off. At 0, the shop can stay open all night. Sleeping closes the shop and sends the
  customers away.
- Daily quests arrive at 6:00.

## How machines read recipes

- **Active machines** (Processor, Assembler, Kiln, …) look up their recipe in `DT_EnemyCrafting` when it's
  chosen or loaded, and keep a copy of the whole row. A change reaches a placed machine once its recipe
  is chosen again, or the save is reloaded.
- **The Grinder** looks for a matching recipe each time an item arrives: its first ingredient and its craft
  type. It keeps only the product's name, so it makes one item per craft whatever `ProductInfo.Count`
  says. A running Grinder keeps the time it started with; a new one uses the changed time. Confirmed in
  game.
- **Raw production** (Stone Crusher, Table Saw, Smelter) uses one unit of the input per product. A larger
  count makes the machine faster, but the total from one input item is still its `MaximumStack` in units.
  Confirmed in game.
- **Side products** (`SideProduct`) are made only by raw-production machines; others drop them. Confirmed
  in game.

## Unit items and recipe cycles

An item with a negative `MaximumStack` holds `-MaximumStack` units (Logs 200, Limestone 150, Iron Ore 100).
Recipes count these items in units. One recipe cycle is `FractionNum` crafts of `CraftingTime` each, so
per whole item:
- amount = `Count × FractionNum ÷ units per item`;
- time = `CraftingTime × FractionNum`.

So the Table Saw (1 unit of Logs → 1 Plank, `FractionNum` 200, 2 s) makes 200 Planks per Log in 400 s.

Quest rewards (`Awards.Products`) count unit items in units: 100 Iron Ore units is one Iron Ore.
Requirements (`RequireProducts`) count whole items.

Fuel: `HeatValue` is heat per unit, and a building's `HeatCost` is heat per second. So fuel items per
second = `HeatCost ÷ (HeatValue × units per item)`, adjusted by the `FuelEfficiency` and
`FactoryConsumeSpeed` attributes. Checked in game: a Crucible (4 heat a second, 4 s per Charcoal) made
12.5 Charcoal from 10 Planks (200 heat). With production speed at +50 % and `FactoryConsumeSpeed` at
+25 %, it ran 1.5 times as fast but still made 12.5, so heat use followed the speed. A changed
`HeatCost` shows in the tooltip, but a Kiln still used its old 15 a second. Steam has a heat value of 20, so a building heated by steam uses
`HeatCost ÷ 20` steam per second.
