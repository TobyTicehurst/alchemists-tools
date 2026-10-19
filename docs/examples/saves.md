# Worked example: the template save

Every new game starts as a copy of a save inside the game, `PreviewSaveGames/DefaultGame.sav`. It holds
the start of a game:
- the first quest (Main1);
- the empty 24-slot inventory;
- the starting unlocks: the Wooden Table, Workbench and Checkout Table, the first research (`Level1`), and
  Logs and Gelatinous Gridlock for sale;
- four loans;
- the starting land and its buildings.

Table edits can't change any of these. A mod's `save/DefaultGame.json` can.

The worked example is `examples/QuickStart/` (in this repo): the first quest asks for 10 Large Wooden Gears instead of 20,
and rewards 500 Flax instead of 200.

| Change | Status |
|---|---|
| Main1 asks for 10 Large Wooden Gears (was 20) | Confirmed in game |
| Main1 rewards 500 Flax (was 200) | Builds; not checked in game |

Template edits only affect **new games**. Existing saves already have their own copy.

## The edit file

`examples/QuickStart/save/DefaultGame.json`:

```json
{
  "edits": [
    {"property": "Count", "after": "RequireProducts", "expect": 20, "value": 10},
    {"property": "Count", "after": "Products", "expect": 200, "value": 500}
  ]
}
```

The save is a long run of named values, the way Unreal saves objects. An edit finds one by its name:
- **`property`**: the name. On its own, it means the first value of that name in the save.
- **`after`**: start looking after the first value of this name. Main1's requirement is the first
  `Count` after `RequireProducts`; its reward is the first `Count` after `Products`.
- **`occurrence`**: take the n-th match instead of the first, counting from 0.
- **`expect`**: the value it has now. The build stops if it differs, so a game update that changes the
  template can't make the edit change something else.
- **`value`**: the new value.
- **`in`**: `"header"` for the save's header (the summary the load menu shows); the body otherwise.

Only **numbers and true/false** can be changed so far. Names, texts and lists would change the save's
length, and the framing around the values isn't understood well enough to rewrite yet. So you can't yet
put an item in an empty inventory slot, or unlock another research.

## Build and install

```
alchemists-tools mod build QuickStart --mods examples
alchemists-tools mod install QuickStart --mods examples
```

```
== template save (save/DefaultGame.json)
   template save: Count after RequireProducts 20 -> 10
   template save: Count after Products 200 -> 500
```

The installed mods' save edits are merged into one replacement template, `zzz_ModSave_P`. When two mods
set the same value differently, the install warns, and the one later in the load order wins. Start a
new game to see the change.

## What else the template holds

These are the numbers and true/false values in the template (game build 25991404) that look useful. All
of them are **untested**: nobody has yet checked what changing them does in game.

| Value | Edit | Now | Probably |
|---|---|---|---|
| Main1's requirement | `"property": "Count", "after": "RequireProducts"` | 20 | Confirmed (QuickStart) |
| Main1's item reward | `"property": "Count", "after": "Products"` | 200 | The Flax given |
| Main1's level-up | `"property": "LevelUp"` | true | Whether completing it raises the level |
| Alchemy level | `"property": "PlayerLevel"` (also `YesterdayLevel`) | 0 | The starting level. Quests and the shop use the level at the start of each day |
| Experience | `"property": "CurrentExp"` | 0.0 | |
| Upgrade points | `"property": "UnallocatedPoints"` | 0 | Points to spend in the upgrade tree |
| Day | `"property": "GameDays"` | 0 | |
| Time of day | `"property": "GameSeconds"` | 612.1 | Seconds since midnight; one game hour is 60 seconds, so 360 is 6:00 |
| Random seed | `"property": "RandomSeed"` | 19947 | New games get a new one anyway |
| Loans | `"property": "Cost"` (`"occurrence"` 0–3 for the four loans); also `LoanDay`, `UnlockLevel` | 2800, 10500, 20000, 42000 | The loans offered. `DT_Loans` has other amounts, and which one a game uses is unknown |
| Inventory slot sizes | `"property": "ItemStack"`, `"occurrence"` 0–23 | 0 | Useless while the slots' items can't be set |

For example, to start a new game at 6:00:

```json
{"property": "GameSeconds", "expect": 612.1000000000028, "value": 360.0}
```

`expect` must be the exact stored value. A wrong one stops the build, and the message shows what's
stored.

There's no command yet that lists the template's values. Ask for one if you need to look further.
