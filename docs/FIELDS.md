# Table and field guide

Every data table in the game, what its rows are, and what each field does. An edit file can change any
of these tables (`docs/MODDING.md`, "Edit files"). The values a table holds now are in
`data/DataTables/<table>.json`, written by `alchemists-tools extract`, with object references as paths.

Statuses, used throughout:
- **Confirmed**: a change was seen to work in game.
- **Known**: what the field does is known from the game's code or its data, but changing it hasn't been
  tried.
- **Unknown**: what it does hasn't been worked out.

Most fields are *Known*. Try a change in a new save before relying on it, and update its status here when you
find out.

**Contents**
- [Value types](#value-types)
- [How changes reach a game](#how-changes-reach-a-game)
- [Items](#dt_enemies-items): `DT_Enemies`
- [Recipes](#dt_enemycrafting-recipes): `DT_EnemyCrafting`
- [Buildings](#dt_buildings-buildings-and-machines): `DT_Buildings`
- [Research](#dt_skillpoints-research): `DT_SkillPoints`
- [Upgrades](#dt_upgradepoints-upgrades): `DT_UpgradePoints`
- [Effects of upgrades, gear and recognitions](#dt_improvements-effects): `DT_Improvements`
- [Attributes](#dt_attributes-attributes): `DT_Attributes`
- [Quests](#dt_quests-quests): `DT_Quests`
- [Purchase contracts](#dt_supplycontract-purchase-contracts): `DT_SupplyContract`
- [Recognitions](#dt_license-recognitions): `DT_License`
- [Workbench unlocks](#dt_workbench-workbench-unlocks): `DT_Workbench`
- [Equipment](#dt_playergear-equipment): `DT_PlayerGear`
- [Plants](#dt_plantseedconfig-plants): `DT_PlantSeedConfig`
- [Land](#dt_gridzones-land): `DT_GridZones`
- [Loans](#dt_loans-loans): `DT_Loans`
- [Customers](#dt_customers-customers): `DT_Customers`
- [Manual pages](#dt_manuals-manual-pages): `DT_Manuals`
- [Achievements](#dt_achievements-achievements): `DT_Achievements`
- [Other tables](#other-tables): railways, elevators, belts and pipes, floors and walls, controls, the
  user interface
- [What isn't in any table](#what-isnt-in-any-table)

## Value types

| Type | Written as | Example |
|---|---|---|
| Whole number | a number without a decimal point | `"Count": 4` |
| Decimal number | a number | `"CraftingTime": 4.0` |
| True/false | `true` or `false` | `"bHideInGame": false` |
| Name | a row name of another table, or `"None"` | `"IngredientName": "WoodBoard"` |
| Choice | one of the names listed for the field | `"CraftType": "Process"` |
| Text | any string. A new text is the same in every language; a text you leave alone stays translated. | `"DisplayName": "Stone Processor"` |
| Asset | a path to a game asset, or to one of your mod's cooked assets (`docs/examples/cooked-content.md`) | `"/Game/Arts/UI/Furnitures/T_UI_Kiln.T_UI_Kiln"` |
| List | `[...]`, replaced whole: give every entry | `"Predecessors": ["Processor"]` |
| Object | `{...}`, merged field by field: give only what changes | `"UnlockCost": {"Z": 300}` |

Objects that appear in many tables:

- **Price** (`IntVector`): `{"X": gold, "Y": silver, "Z": copper}`. One silver is 1,000 copper and one
  gold is 100 silver. The parts add up, and each may be larger than a coin's worth (`{"Z": 6000}` is
  6 silver).
- **Item and count** (`BeltTDIngredientCount`): `{"IngredientName": "WoodBoard", "Count": 2}`. Unused:
  `{"IngredientName": "None", "Count": 0}`.
- **Unlock** (`BeltTDConfigName`): `{"ConfigName": <row>, "ConfigType": <which table>}`. The types are
  `Ingredients` (an item, `DT_Enemies`), `SkillPoints` (a research), `CraftingRecipes`
  (`DT_EnemyCrafting`), `ConstructOptions` (a building, `DT_Buildings`), `Improvements`
  (`DT_Improvements`), `Gear` (`DT_PlayerGear`), `Contract` (`DT_SupplyContract`) and `License`
  (`DT_License`).
- **Point** (`IntPoint`): `{"X": 0, "Y": 3}`. **Grid cell** (`IntVector`): `{"X": 0, "Y": 0, "Z": 0}`.
- **Tags** (`GameplayTagContainer`): a list of tag names, such as
  `["Building.Crafting.ActiveCrafting"]`.

**Unit items.** An item with a negative `MaximumStack` is a *unit item*: one item holds
`-MaximumStack` units (Logs 200, Limestone 150, Iron Ore 100). Recipes and quest rewards count these
items in units. Quest requirements count whole items.

## How changes reach a game

- **The tables load when the game starts.** Edit, install, then restart the game.
- **A new game starts from a template save**, not from the tables:
  - the first quest (Main1);
  - the starting inventory, unlocks, land and buildings;
  - a copy of the loans.

  Changing those takes a save edit (`docs/examples/saves.md`).
- **Saves keep what was unlocked.** Finishing a research copies its unlock into the save, so changing
  what a research unlocks has no effect on saves that already have it.
- **Spawned quests keep their own copy** of what they ask for and reward. A table change applies to
  quests spawned after it.
- **Never remove a building row that's been released**, or rename it. A save with a placed building
  whose row is missing crashes on load. Missing recipes and researches don't crash: a machine's recipe
  just becomes unset.

## DT_Enemies: items

180 rows, one per item, including liquids and gases (the game's code began as a tower-defence prototype,
so it calls items "enemies"). Row names are item IDs (`WoodBoard` is Planks). The English names are in
`DisplayName`; several IDs share one.

| Field | Type | What it does | Status |
|---|---|---|---|
| `ID` | whole number | The item's number. `ID ÷ 100` is its tier for daily quests and the shop when `QuestTier`/`ShoppingTier` is 0 | Known |
| `DisplayName` | text | The name shown | Known |
| `DisplayIcon` | asset (`Texture2D`) | The icon | Known |
| `SolidMesh` | asset (`StaticMesh`) | The model of one item, on belts and in the world | Known |
| `ShowColor` | `{R, G, B, A}` | Unknown; `A` looks like a category | Unknown |
| `IngredientTags` | tags | `Ingredient.Type.Solid`, `.Mash`, `.Herb`, … | Unknown |
| `bHideInGame` | true/false | Hidden: not offered by quests or the shop | Known |
| `bHideInCreative` | true/false | Hidden in Creative mode | Known |
| `MaximumStack` | whole number | Stack size. Negative: a unit item, holding `-MaximumStack` units | **Confirmed** (Limestone −150 → −300 gave 300 Stone per Limestone) |
| `StockCost` | price | What it costs from the merchant or the purchasing portal (raw materials) | Known |
| `SellType` | choice: `Worthless`, `Groceries`, `Jewelry`, `Remedies`, `Artcrafts`, `Vehicles`, `Liquid` | The shop category customers pick from | Known |
| `CostValue` | price | The base price in your shop | Known |
| `SellCanStack` | true/false | Unknown | Unknown |
| `HeatValue` | decimal | Heat per unit as fuel (Logs 10 per unit, 2,000 per item) | Known |
| `NutrientValue`, `NutrientSpeed` | decimal | Fertilizer value, and how fast it's used | Known |
| `IsLiquid` | true/false | A liquid: carried in pipes | Known |
| `RandomQuestBaseNum` | whole number | Base amount daily quests ask for; 0 = never asked for | Known |
| `QuestCost` | whole number | Value per item in quest rewards (copper) | Known |
| `QuestTier`, `ShoppingTier` | whole number | Tier for daily quests and the shop (0: from `ID`). An item is offered at tiers up to the player's level + 1 | Known |
| `NpcShoppingBaseNum` | whole number | The most a customer buys of it at once | Known |
| `BaseCost` | decimal | The value the wiki shows as "cost" | Unknown |
| `CauldronCost`, `CauldronTarget`, `CauldronMulti` | decimal | Cauldron mechanics | Unknown |
| `BulkMeshThreshold`, `BulkMeshList` | lists | Models for piles of the item, by count | Known |
| `DescRowName` | name | Another item whose description this one shows | Unknown |
| `AllowPortalSupply` | true/false | Probably whether the portal can supply it | Unknown |

Coins (`CopperCoin`, `SilverCoin`, `GoldCoin`) are items, but their worth is fixed in the game's code.

## DT_EnemyCrafting: recipes

161 rows, one per recipe. Row names are recipe IDs; most are named after their product. A row name the
table doesn't have makes a new recipe (`docs/examples/recipes.md`).

| Field | Type | What it does | Status |
|---|---|---|---|
| `ID` | whole number | The recipe's number; a new recipe gets a free one | Known |
| `IngredientList` | list of item and count | What one craft uses. Unit items count in units | **Confirmed** (Brew Barrel) |
| `ProductInfo` | item and count | What one craft makes | **Confirmed** on active machines and the Extractor; see below |
| `CraftType` | choice, below | Which machines run it | **Confirmed** |
| `CraftingTime` | decimal | Seconds per craft at 100 % speed | **Confirmed** |
| `FractionNum` | whole number | Crafts per cycle, for unit items: Table Saw 1 Logs → 200 Planks is 200 crafts of 2 s | Known |
| `SideProduct` | item and count | Always made as well. Works only on raw-production machines; others discard it | **Confirmed** |
| `FailRate1`, `FailProduct1`, `FailRate2`, `FailProduct2` | decimal, item and count | Alternative outcomes (Athanors): the main product's chance is `1 − FailRate1 − FailRate2` | Known |
| `CatalystCost`, `ProductSequence`, `UnstableSequence` | whole number, lists | Catalyst mechanics: a fixed cycle of outcomes (0 = main product, 1 and 2 = fail products) | Known |
| `bHideInGame` | true/false | Hidden | Known |
| `bAlternate` | true/false | An alternative recipe for an item another recipe makes | Known |

How machines read a recipe:
- **Active machines** (Processor, Assembler, Kiln, …) copy the whole recipe when it's chosen or loaded.
  Every field works.
- **The Grinder** (passive) makes one item per craft, whatever `ProductInfo.Count` says. Its time works
  on Grinders built after the change.
- **Raw production** (Table Saw, Stone Crusher, Smelter): a larger count makes it faster. The total per
  input item comes from the input's `MaximumStack`.

Craft types, and the machines that run them:

| Craft type | Machines | Craft type | Machines |
|---|---|---|---|
| `TableSaw` | Table Saw | `Assemble` | Assembler |
| `StoneCrusher` | Stone Crusher | `Blend`, `AdBlend` | Blender, Advanced Blender |
| `Smelter` | Iron Smelter | `Distill`, `AdDistill` | Alembic, Advanced Alembic |
| `Plant` | Seed Plot, nurseries | `Athanor`, `AdAthanor` | Athanor, Advanced Athanor |
| `Grind` | Grinder, Enhanced Grinder | `OmniCraft` | Omni-Machine |
| `Extract` | Extractor | `Shaper`, `AdShaper`, `AtShaper` | Shaper, Advanced Shaper, Aether Shaper |
| `Calcinate` | Crucible | `Paradox` | Paradox Crucible |
| `Refine` | Refiner | `Cauldron` | Cauldron, Advanced Cauldron |
| `Fire` | Kiln | `AdProcess` | Arcane Processor |
| `Process` | Processor | `LiquidTap` | Brew Barrel |
| `Max` | none: free for a machine of your own | 26–255 | none: free (`new-craft-types` in `mod.json`) |

A machine's craft type is set in its Blueprint, not in a table (`docs/examples/blueprints.md`).

Port checks: solid ingredients and products need belt connections on the machine, and liquids need pipe
connections (`DT_Buildings.InOutList`). The build warns when a recipe needs a connection its machines
don't have.

## DT_Buildings: buildings and machines

299 rows: every building, machine, belt, wall, shelf and decoration, plus hidden parts. Row names are
building IDs (`Processor`, `Kiln`, `SmallContainer`). A row with `_copy` makes a new building
(`docs/examples/buildings.md`).

| Field | Type | What it does | Status |
|---|---|---|---|
| `ID` | whole number | Groups buildings in the build menu, sorted by ID: 1xx passive crafting, 2xx active crafting, 3xx heating, 4xx shop furniture, 6xx construction, 7xx raw production and portals, 8xx containers, 9xx frames and shelves, 10xx–12xx decorations, 90xx hidden parts. Not unique | **Confirmed** (a new row with ID 220 appeared last among the active crafting machines) |
| `DisplayName`, `DisplayDesc` | text | Name and description | **Confirmed** (the name) |
| `DisplayIcon` | asset (`Texture2D`) | Build-menu icon, 512 × 512 | Known |
| `FurnitureType` | choice: `Logistics`, `Normal`, `CanInteract`, `Construct`, `ConstructV2`, `Pipeline`, `Railroad` | Kind of building; machines are `CanInteract` | Known |
| `SecondaryType`, `SecondaryIndirect` | choice, whole number | For belts, floors, elevators and similar: which of the [other tables](#other-tables) describes it, and its row there | Known |
| `bHideInGame`, `bHideInCreative` | true/false | Hidden from the build menu | Known |
| `HeatCost` | decimal | Heat used per second (fuel use = `HeatCost ÷ HeatValue`) | Known |
| `DisableRotator` | true/false | Can't be rotated | Known |
| `ScriptClass` | asset (Blueprint class) | The Blueprint spawned: the building's model, parts and behaviour | **Confirmed** |
| `InstanceMesh`, `bInstanceAffectsNavigation` | asset, true/false | A model drawn instanced (blocks, belts) | Unknown |
| `CollisionProfile` | choice: `Enemy`, `Furniture`, `FurnitureNoBuild`, `FurnitureMove`, `BuildingSolid` | How it collides | Known |
| `Dilation` | `{TimeDilation, SizeDilation}` | Not 1 only on belt slopes, tunnels and drawbridges. It isn't a machine's speed: every machine has 1 | Unknown |
| `BeltRotation` | decimal | Belt angle (−45 on slopes) | Unknown |
| `GridConfigList`, `VolumeList`, `PhantomGridConfigs`, `ReplacementRegions` | lists | The cells the building occupies, and how | Known |
| `InOutList` | list | Belt and pipe connections (below) | Known |
| `CostList` | list of item and count | Build cost | Known |
| `BuildingTags` | tags | `Building.Crafting.ActiveCrafting`, `.PassiveCrafting`, `.RawProduction`, `Building.Heating`, `Building.Logistics.Storage`, … | Known |
| `AlterBuildOption` | name | The building's mirrored or alternative form (`Processor` ↔ `Processor_Sym`). Set `"None"` on a copy | Known |
| `ActiveSound` | asset | Sound while working | Known |

An `InOutList` entry is one connection point:
- `Position`: the cell it's on;
- `IsPipeGrid`: a pipe connection rather than a belt;
- `PipeOverrideType` (`InputAndOutput`, `OnlyInput`, `OnlyOutput`), and `MaxLiquidValue`;
- `Left`, `Right`, `Up`, `Bottom`: for each side, `IsInput` and `IsOutput`;
- `Vertical`: `Base` and `Top`;
- `InOutType`, `Color`, `OffsetOutput`.

`data/machines.json` counts each machine's ports.

Not in this table:
- which recipes a machine runs: its Blueprint's `FactoryCraftType`;
- how much a chest holds: its Blueprint's container capacity;
- a machine's own speed: there's none, only the global `FactorySpeed` attribute.

## DT_SkillPoints: research

154 rows: the research tree (the Z panel). Row names are research IDs, mostly named after what they
unlock. Most unlock one thing; the `Level1`–`Level10` rows raise the technology level.

| Field | Type | What it does | Status |
|---|---|---|---|
| `Tier` | whole number | The tree's band | Known |
| `LevelSkill` | true/false | A technology-level node (`Level1`–`Level10`) | Known |
| `UpgradeSkill` | true/false | Always false here (true in `DT_UpgradePoints`) | Known |
| `SlotPosition` | point | Place in the tree: X left/right, Y down. Links are drawn downwards, so put a node at least one row below its predecessors | **Confirmed** |
| `Predecessors` | list of names | Researches needed first | **Confirmed** |
| `UnlockCost` | price | Cost to research | Known |
| `UnlockResearchPoints` | decimal | Research time in seconds at 100 % Research Speed | Known |
| `UnlockItem` | unlock | What it unlocks: a recipe (`CraftingRecipes`) or a building (`ConstructOptions`) | **Confirmed** (both) |
| `LevelUnlockItems` | list of names | Items that become purchasable | Known |
| `ExtraUnlockConstructions` | list of names | More buildings it unlocks | Known |
| `IsUnlimited`, `MaxUnlimitedLevel` | true/false, whole number | Repeatable (upgrades only) | Known |
| `Deprecated` | true/false | Hidden from the tree. Setting it back to `false` didn't make a node appear | **Confirmed** (doesn't revive a node) |

Every new recipe and building needs a research that unlocks it. `Level1` is already done in a new game,
so changing what it unlocks has no effect.

## DT_UpgradePoints: upgrades

130 rows: the upgrade tree (Logistics, Factory, Fuel, …), with the same fields as research. Each row's
`UnlockItem` is one `DT_Improvements` row, of the same name.

| Field | What it does here |
|---|---|
| `UnlockResearchPoints` | **The cost in upgrade points**: 60 × tier² (60, 240, 540, …), and 10,140 for tier 13 |
| `UnlockCost` | Not a cost: `Z` counts rows. Ignore it |
| `IsUnlimited`, `MaxUnlimitedLevel` | Tier 13 repeats, up to 80 times |
| `SlotPosition`, `Predecessors`, `Deprecated` | As for research |

## DT_Improvements: effects

194 rows: what upgrades, equipment and recognitions do. Each row is a list of changes to
[attributes](#dt_attributes-attributes). Upgrades use the row of the same name; equipment and
recognitions name theirs (`UnlockedBuffs`, `UnlockBuff`).

| Field | Type | What it does | Status |
|---|---|---|---|
| `DisplayName`, `DisplayIcon` | text, asset | Shown in the upgrade's tooltip | Known |
| `ImprovementLevel` | whole number | The level shown | Known |
| `AllowRepeat` | true/false | Can be had more than once (tier 13) | Known |
| `Effects` | list of `{AttributeName, ModificationType, ModValue}` | The changes. `AttributeName` is an attribute's row name. `ModificationType`: `Add` (a flat amount) or `Increase` (percentage points on a % attribute); the game's rows use only these two. `Multiply` and `Override` also exist | Known |
| `Deprecated` | true/false | Unused | Known |

## DT_Attributes: attributes

34 rows: the player's and the world's numbers, with their base values. Improvements change them.

| Row | Shown as | Base | Row | Shown as | Base |
|---|---|---|---|---|---|
| `InventoryCapacity` | Inventory Capacity | 24 | `FactorySpeed` | Device Production Speed | 100 % |
| `SprintSpeed` | Sprinting Speed | 700 % | `FactoryConsumeSpeed` | Heat Consuming Speed | 100 % |
| `StoreReputation` | Shop Reputation | 0 | `FuelEfficiency` | Fuel Heat Value | 100 % |
| `ConveyerSpeed` | Conveyor Belt Speed | 60 /min | `FertilizerEfficiency` | Fertilizer Nutrient Value | 100 % |
| `ConveyerLiftSpeed` | Conveyor Lifter Speed | 60 /min | `ResearchPoints` | Research Speed | 100 % |
| `CatapultSpeed` | Catapult Fire Rate | 60 /min | `CannonSpeed` | Cannon Fire Rate | 120 /min |
| `RestockSpeed` | Restocking Speed | 5 /s | `CheckoutSpeed` | Checkout Speed | 1 /s |
| `SleepTime` | Sleep Time | 8 hr | `GarbageSellPrice` | Trash Recycling Price | 100 % |
| `ExtractorSkill` | Extractor Output | 100 % | `AlembicSkill` | Alembic Output | 100 % |
| `AthanorSkill` | Athanor Failure Rate | 0 % | `AltarEfficiency` | Relic Withdrawal Bonus | 0 % |
| `StoreProfit` | Shop Profit | 100 % | `QuestProfit` | Quest Rewards | 100 % |
| `GeneralGoodsProfit` | Sundries Profit | 100 % | `PotionProfit` | Potion Profit | 100 % |
| `LiquidProfit` | Beverage Profit | 100 % | `JewelProfit` | Jewelry Profit | 100 % |
| `RelicProfit` | Relic Profit | 100 % | `ContractProfit` | Purchase Contract Reward Bonus | 0 % |
| `ContractNum` | Purchase Contract Amount Bonus | 0 % | `ExtraQuest` | Daily Random Quest | 1 |
| `CustomerBuyNum` | Customer Purchase Increase | 0 % | `PortalTrafficRate` | Grand Portal Traffic Rate | 100 % |
| `UnlockBlueprintTool` | Save Blueprint | 0 | `BlueprintCostMoney` | Blueprints for Money | 0 |

Fields: `DisplayName`, `AttributeDesc`, `AttributeUnit` (texts), `BaseValue` (decimal), and
`UnlockAttribute` (true for the two "Blueprint" switches). **Unknown** whether a changed `BaseValue`
reaches a game. Some values, such as the inventory's 24 slots, are also stored in the template save.

## DT_Quests: quests

24 rows:
- `Main1`–`Main9`: the main quests. Each one done raises the alchemy level by 1.
- `Carpenter1`–`Carpenter6`: Carpenter Requests.
- `Contract1`–`Contract6`: Sample Requests, which unlock purchase contracts.
- `RandomNormal`, `RandomUrgent`, `RandomBulk`: the templates for daily quests, whose items are picked
  each day.

| Field | Type | What it does | Status |
|---|---|---|---|
| `Title`, `Message` | text | Shown in the quest window | Known |
| `RequireProducts` | `{Items, Nums}` | Items to hand in, and how many of each (whole items). For daily templates the lengths of the two lists give how many items are picked | **Confirmed** (Main2: 50 → 25 Linen Rope) |
| `Awards` | object | `Gold` (price), `Products` (items; unit items in units), `Buildings` and `Contracts` (unlocked), `LevelUp`, and `GoldFactor` (the daily quests' reward multiplier) | Known |
| `Conditions` | object | `RequireQuests` (quests done first), `RequireContracts` (items already sent under the previous contract), `GameLevelRange` and `DateRange` (when it can appear) | Known |
| `DateRange` | point | How long a quest stays: it ends after `Y + 1` days (`Y` = −1: never) | Known |
| `RandomWeight` | decimal | Chance of each daily template (Normal 3, Urgent 1, Bulk 1) | Known |
| `IsMain`, `AutoAccept`, `Deprecated` | true/false | Main quest; accepted at once; unused (Contract2–6 are marked so but used) | Known |

`Main1` is in the template save, so a table change doesn't reach it (`docs/examples/saves.md`). How
daily quests pick items and rewards is in `docs/GAME-MECHANICS.md`, "Daily quests".

## DT_SupplyContract: purchase contracts

6 rows, named after the item each contract buys (`Bandage`, `GloomSpores`, …). Unlocked by Sample
Requests.

| Field | Type | What it does | Status |
|---|---|---|---|
| `SellItem` | item and count | The item, and how many one contract takes | Known |
| `ContractReward` | price | Paid per contract | Known |
| `DailyLimit` | whole number | Contracts per day | Known |

## DT_License: recognitions

37 rows, the Recognition panel: `GeneralGoodsLicense1`–`5`, `ApothecaryLicense1`–`5`, and so on.

| Field | Type | What it does | Status |
|---|---|---|---|
| `LicenseType`, `LicenseTier` | choice (`General`, `Potion`, `Jewellery`, `Relic`, `Minting`, `Tavern`, `Supplier`, `Alchemist`), whole number | Which column and tier | Known |
| `LicenseText`, `UseSpecialText` | text, true/false | The name | Known |
| `DisplayIcon`, `DisplayBackground` | assets | The badge | Known |
| `PreLicenseName`, `PreSkillName` | names | The recognition and the research needed first | Known |
| `RequestConditions` | list of `{Type, TargetName, TargetCount}` | Requirements: `ProduceAll`, `SellAll` or `ContractAll` of an item | Known |
| `ApplyPrice` | price | The fee | Known |
| `UnlockItems` | list of unlocks | What it unlocks (Supplier rows unlock contracts) | Known |
| `UnlockBuff` | name | Its `DT_Improvements` row. Every recognition also adds 8 Shop Reputation | Known |

## DT_Workbench: workbench unlocks

81 rows: the workbench's Unlocks tab. The row name is the building it unlocks.

| Field | Type | What it does | Status |
|---|---|---|---|
| `UnlockBuilding` | name | The building (same as the row name) | Known |
| `UnlockSkillName` | name | Research needed first | Known |
| `PrerequisiteBuilding` | name | Workbench unlock needed first | Known |
| `UnlockCost` | list of item and count | Cost | Known |
| `ExtraUnlockBuildings` | list of names | More buildings unlocked with it | Known |

## DT_PlayerGear: equipment

19 rows: the workbench's Equipment tab: `Backpack1`–`6`, `PotionBelt1`–`5`, `BuildTool1`–`4`, `Gloves1`–`4`. Each
tier needs the one before.

| Field | Type | What it does | Status |
|---|---|---|---|
| `GearType`, `Tier` | choice (`Backpack`, `Gloves`, `PotionBelt`, `BuildTool`, `Blueprint`, `Monocle`), whole number | Which piece and tier | Known |
| `UpgradeCost` | list of item and count | Cost | Known |
| `UnlockedBuffs` | list of names | Its `DT_Improvements` rows (`Bag1`: +4 inventory slots) | Known |
| `UnlockedBuildings` | list of names | Buildings it unlocks | Known |

## DT_PlantSeedConfig: plants

9 rows, one per seed (`FlaxSeed`, `SageSeed`, …, `TreeStage1`–`3`).

| Field | Type | What it does | Status |
|---|---|---|---|
| `PlantName` | name | The item it grows | Known |
| `GrowthSeconds` | whole number | Time to grow | Known |
| `GrowthNutrientValue` | decimal | Fertilizer needed (the "V" in game) | **Confirmed** (Flax 24 → 12; its tooltip still said 24, but that was with the edit made while the game ran) |
| `GrowthNum` | whole number | How many it yields | Known |
| `SideProductName`, `SideGrowthNum` | name, whole number | A second product, and how many | Known |
| `SeedMesh`, `PlantMesh` | assets | Models | Known |

## DT_GridZones: land

20 rows: `Room1_1` (the starting land), `Room1_2`, `Land1`–`Land13`, `HighLand1`–`4` and `AloneLand`.

| Field | Type | What it does | Status |
|---|---|---|---|
| `DisplayName` | text | Name | Known |
| `UnlockCost` | price | Price | Known |
| `DailyRent` | whole number | Rent per day, in copper | Known |

## DT_Loans: loans

8 rows, named `NewRow`, `NewRow_0`, … in unlock order. Loans no longer charge interest.

| Field | Type | What it does | Status |
|---|---|---|---|
| `LoanCurrency` | price | Amount | Known |
| `UnlockLevel` | whole number | Alchemy level needed | Known |
| `LoanDay` | whole number | Term in days; probably unused now | Unknown |
| `CostRateRange` | `{X, Y}` | The old interest rate, unused | Known |
| `Title`, `Message` | text | Shown | Known |

The template save holds its own list of four loans, with different amounts (2,800 copper for the
first). **Unknown** which one a new game uses.

## DT_Customers: customers

5 rows: `Villager`, `Soldier`, `Monk`, `Merchant`, `Noble`.

| Field | Type | What it does | Status |
|---|---|---|---|
| `UnlockLevel` | whole number | Alchemy level from which they come | Known |
| `BasePr`, `RepPrMulti` | decimal | How often they come: weight `BasePr + reputation × RepPrMulti` | Known |
| `IdlePr` | decimal | Weight for idle wandering visitors, who buy nothing | Known |
| `GoodsPrMap` | map of shop category → weight | Which categories they buy from | Known |
| `PurchaseNumberCurve` | asset (curve) | How many items they pick; the curves are `CV_NormalPurchase` and `CV_MerchantPurchase` | Known |
| `CustomerClasses`, `ClassPr` | list of Blueprint classes, list of decimals | Their models, and the chance of each | Known |

`docs/GAME-MECHANICS.md`, "Customers", has the formulas. The curves themselves aren't data tables, and edit files can't
change them yet.

## DT_Manuals: manual pages

29 rows, the in-game manual: `ManualTitle`, `ContentBeforeVideo` and `ContentAfterVideo` (texts, with
key images such as `<img id="Key_W"/>` from `DT_RichTextImage`), `VideoShowcase` (asset), `HideInGame`,
and when the page opens itself:
- `AutoTrigger` and `ForceWatch`;
- `TriggerSkillName`: after a research;
- `TriggerUnlockName`: after an unlock.

## DT_Achievements: achievements

55 rows: Steam achievements (`Type`, `TargetName`, `TargetNum`, `AchievementStr`). Leave them alone:
they're tied to Steam.

## Other tables

Less studied, and none changed in game yet. All can be edited like the tables above; their row names
are often numbers.

| Table | What it holds |
|---|---|
| `DT_Trolleys` | The railway wagon: `Speed`, `MinSpeed`, `MaxSpeed` (all 120), models |
| `DT_Railroads`, `Indirect/Railroad/DT_RailroadConfig` | Railway track shapes, and which rows are left and right turns |
| `Indirect/Elevator/DT_Elevator`, `DT_FreightElevator` | The building rows that make up each elevator |
| `Indirect/Conveyer/DT_ConveyerLiftConfig` | The conveyor lifter's parts |
| `Indirect/Expandable/DT_ExpandableBuilding` | Buildings that extend (beams, tunnels, chains, chandeliers): `MaxBodyNum`, the most segments (8–10) |
| `Indirect/Construct/DT_ConstructBuilding`, `DT_ConstructNormals`, `Indirect/ConstructV2/DT_ConstructV2` | Floors and walls: sizes, edge pieces, and `RiskLoad`/`MaxLoad` |
| `DT_BeltMeshBuildings`, `DT_PipeMeshConfig2` | The belt or pipe model for each shape of connection |
| `DT_InputKeyMapping`, `DT_InteractionInfos` | Controls, and the prompts shown for them |
| `DT_RichTextImage`, `DT_RichTextStyle` | Images and styles used in the user interface's texts |
| `DT_DefinedTutorials`, `Blueprints/Gameplay/Tutorials/DT_UMPI_IndicatorSettings_DataTable` | The tutorial's steps and markers |
| `DT_SkillMerge` | Research and upgrades combined. Edit `DT_SkillPoints` and `DT_UpgradePoints` instead |
| `TempTables/DT_TempCrafting`, `DT_TempEnemy` | Old copies of the recipes and items, unused |

`alchemists-tools extract` also writes some curves (`CV_*`) and tables from the engine's sample content.

## What isn't in any table

- **A coin's worth**: in the game's code.
- **Which recipes a machine runs** (its craft type), and a chest's capacity: in its Blueprint.
- **The start of a new game**: the template save (`docs/examples/saves.md`).
- **The machine glyph in research tooltips**: not in any table found so far.
- **The 5,000-building limit when deconstructing**: not in any table or C++ header.
- **Daily quest and customer logic**: in the game's code, driven by the fields above
  (`docs/GAME-MECHANICS.md`).
