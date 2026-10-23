# Worked example: Lua mods (UE4SS)

Table edits change the game's data. To change what the game *does* while it runs, a mod needs code.
[UE4SS](https://docs.ue4ss.com/) loads Lua scripts into the game. Those scripts can:
- read and change the game's objects;
- hook its functions;
- react to new objects;
- add key binds.

The worked example is `examples/ExampleBagReporter/` (in this repo). Deconstructing buildings puts what they held into a bag;
the mod logs each new bag, and on Ctrl + Numpad 1 lists what the newest one holds. It only reads.

| Part | Status |
|---|---|
| Noticing new bags (`NotifyOnNewObject`) | Confirmed in game (VaultProbe, 2026-10-18) |
| Reading a bag's contents (`Library.contents`) | Confirmed in game (VaultProbe) |
| This mod as a whole | Builds; not run in game |

## Setting up UE4SS

1. Install UE4SS with the tools: Setup's **Install UE4SS** in the GUI, the installer's offer, or
   `alchemists-tools ue4ss install`.
2. Start the game once.
3. Check the log, `Binaries/Win64/ue4ss/UE4SS.log`: everything a mod prints goes there.

The game runs on Unreal Engine 5.7, which only UE4SS's experimental builds support. The tools install one build,
`v3.0.1-1164-g5e627997` (pinned in `core/src/ue4ss.rs`), the one this game's mods are made and tested with: C++ mods
are compiled against an exact build, so every player needs the same one. They download it from their own release
(or else UE4SS's `experimental` release) and check it against its SHA-256. A UE4SS installed some other way is left
alone; if it's that very build, the tools take it over as it is, so they can remove it later. **Disable** in Setup
(or `alchemists-tools ue4ss disable`) starts the game without UE4SS, as after a game update it can't handle yet.

Installing by hand works too: unzip `UE4SS_v3.0.1-1164-g5e627997.zip` from UE4SS's `experimental` release into
`AlchemyFactory/Binaries/Win64/` (its `experimental-latest` release holds only the newest build).

Modders also use UE4SS to dump the game's `.usmap` (Ctrl + Numpad 6 in a new save); players get the tools' own.

**With AF Mod Loader.** [AF Mod Loader](https://github.com/danipr25/af-modloader) is another way to run Lua mods. Its
installer installs UE4SS 1181, not the build the tools pin (C++ mods made for 1164 may not load with it), with a
loader for mods in `<game>/mods/`. It refuses to install over a UE4SS it
didn't install, so install it first if you want both. Mods installed by `alchemists-tools` still work beside it.
Its installer leaves out UE4SS's sample mods, `Keybinds` among them, so Ctrl + Numpad 6 and the other dump keys
do nothing. To dump the `.usmap`, copy `Mods/Keybinds/` from the UE4SS zip into `ue4ss/Mods/` and put an empty
`enabled.txt` in it (not tried).

## A mod's Lua files

```
examples/ExampleBagReporter/
  mod.json                     "dependencies": ["AlchemistsLibrary >= 0.2.4"]
  ue4ss/Scripts/main.lua       -> ue4ss/Mods/ExampleBagReporter/Scripts/main.lua
```

`alchemists-tools mod install` copies `ue4ss/` to `ue4ss/Mods/<name>/` and enables it (`enabled.txt`). A
mod's `ue4ss-shared/<Name>/` folder goes to `ue4ss/Mods/shared/<Name>/`, where other mods can `require` it.
The build stops when a script requires another mod's module without depending on that mod.

## The shared library

AlchemistsLibrary is a Lua module for every mod to use; its source is the repo's
`ue4ss/AlchemistsLibrary/`. Depend on it in `mod.json`, and install with `--with-dependencies`: the tool
writes its own copy into the mods folder when the folder doesn't have one.

```lua
local Library = require("AlchemistsLibrary")
local log = Library.logger("MyMod")
log.log("hello")                -- UE4SS.log: [MyMod] hello
```

| Function | What it does | Status |
|---|---|---|
| `Library.logger(tag)` | A logger: `log(message)`, `once(message)` (each message only once), `error(message)` | Confirmed |
| `Library.safely(logger, work, ...)` | Runs `work`, logging an error instead of stopping the mod | Confirmed |
| `Library.on_game_thread(logger, work)` | Runs `work` on the game thread, where game objects may be used | Confirmed |
| `Library.keybind(key, modifiers, logger, work)` | A key bind whose work runs on the game thread | Confirmed |
| `Library.list(array)` | A game array as a Lua list | Confirmed |
| `Library.describe(value, layout)` | A readable form of any value, for logs | Confirmed |
| `Library.building_row(object)` | The `DT_Buildings` row of a placed building, or of one of its components | Confirmed |
| `Library.contents(actor, report)` | What a chest, bag or machine holds: `{{name = "WoodBoard", count = 12}, ...}` | Confirmed on bags |
| `Library.components(actor, class_path)` | An actor's components of a class | Confirmed |
| `Library.placed(class_name)` | Every object of a class in the world | Untested |
| `Library.data_table(name)`, `Library.find_row(table, row)` | A data table, or one of its rows, as the game has it in memory | Confirmed (`DT_Buildings` rows and their `CostList`) |
| `Library.sorted_keys(t)`, `Library.by_position(actors)` | A fixed order, for multiplayer (below) | Untested |
| `Library.has_authority(actor)` | Whether this machine is the host for an actor | Untested |

## The example, line by line

`examples/ExampleBagReporter/ue4ss/Scripts/main.lua`:

```lua
local Library = require("AlchemistsLibrary")
local log = Library.logger("ExampleBagReporter")

local BAG = "/Game/Blueprints/Buildings/BP_PhantomTemporaryBag.BP_PhantomTemporaryBag_C"
local newest = nil

NotifyOnNewObject(BAG, function(bag)
    Library.safely(log, function()
        newest = bag
        log.log("a bag appeared: " .. Library.describe(bag))
    end)
end)
```

`NotifyOnNewObject` calls the function as each object of the class is made. That includes the bags
already in a save, while it loads. So the function only notes the bag: reading the world's objects while
it loads can crash the game (Rules, below).

```lua
Library.keybind(Key.NUM_ONE, {ModifierKey.CONTROL}, log, function()
    if not newest or not newest:IsValid() then
        log.log("no bag yet: deconstruct something first")
        return
    end
    local steps = {}
    local held = {}
    for _, item in ipairs(Library.contents(newest, function(line) steps[#steps + 1] = line end)) do
        held[#held + 1] = item.count .. " " .. item.name
    end
    log.log("the newest bag holds: " .. (#held > 0 and table.concat(held, ", ") or "nothing"))
    -- For "nothing", how the bag was read tells an empty bag from one the library couldn't read
    if #held == 0 then
        for _, line in ipairs(steps) do log.log(line) end
    end
end)
```

The bag is read only when the key is pressed, by which time the world has loaded. `IsValid` checks that
the bag still exists. Always check an object you kept from earlier: the game may have removed it since.
`Library.contents` reports each step of its lookup to the function it's given; the mod logs those steps
only when it finds nothing, to tell an empty bag from one that couldn't be read.

Install it, and its library:
```
alchemists-tools mod install ExampleBagReporter --with-dependencies --mods examples
```

In game, deconstruct a chest that holds something, then press Ctrl + Numpad 1. `UE4SS.log` shows:
```
[ExampleBagReporter] a bag appeared: ...BP_PhantomTemporaryBag_C...
[ExampleBagReporter] the newest bag holds: 600 WoodBoard, 200 Brick, ...
```

## Finding out what the game does: probes

To change something, first find which of the game's functions are involved, and when they're called. A
probe is a generated mod that logs calls:

```
alchemists-tools probe VaultProbe "BeltTDInventoryComponent:Server_*" --new-object /Game/Blueprints/Buildings/BP_PhantomTemporaryBag.BP_PhantomTemporaryBag_C
alchemists-tools mod install VaultProbe --with-dependencies
```

- Each argument is `<Class>:<Function>`, with `*` allowed in the function's name.
- `--new-object` also logs each new object of a class.
- The probe logs each call's arguments, named and laid out from the `.usmap`, and after the call, what
  it wrote to its out parameters. It logs return values only for functions without parameters: for the
  others, UE4SS hands the post-hook something else in the return value's place (a test found the
  function's own argument there).
- `--limit` caps the calls logged in full per function (50). `--repeats` (3) caps those whose arguments
  look the same once numbers are set aside, so a function called every tick logs a few calls and then
  only the ones that differ, or that return something new. Every call is still counted.
- Ctrl + Numpad 3 logs how often each function was called, and Ctrl + Numpad 2 what the new objects
  hold.

That probe, VaultProbe, is the one that found the bags.

The game's class and function names are in the UE4SS header dump
(`ue4ss/UHTHeaderDump/BeltTD/Public/*.h`; Ctrl + Numpad 9 in game). FModel shows the game's Blueprints.

## Changing tables while the game runs

A script can change a table in memory:

```lua
local row = Library.find_row("DT_EnemyCrafting", "WoodGear")
if row then row.ProductInfo.Count = 4 end
```

That's how the first table edits were tested. But a `.pak` mod (an edit file) is simpler and more
reliable:
- the game reloads some tables when a world loads, which undoes changes made earlier;
- some systems copy what they need when a game starts;
- tooltips can keep the old values.

Use Lua for what tables can't do.

## Rules

- **Don't touch the world's objects in bulk while a world loads.** Scanning every component during a load
  crashed the game inside UE4SS. React to new objects lightly, and do the real work later: on a key press,
  or with `ExecuteWithDelay`.
- **Calling a game function can crash where hooking it can't.** The C++ behind a function may expect a
  loaded save. `BeltTDPlayerController:GetCurrentStateName()` called from a timer at the main menu (the
  controller has no state yet) crashed the game. To know the state, post-hook it instead: the user
  interface calls it on every change, and as it takes no parameters, the post-hook gets its real return
  value.
- **Use UE4SS's game-thread timers, not `LoopAsync` or `ExecuteWithDelay`.** Those two run their callback on UE4SS's
  async thread (UE4SS 3.0.1 marks them deprecated). A UI script that called `ExecuteWithDelay` often, queuing work
  for the game thread from there, crashed the game inside UE4SS after the log line `Ref was not function`
  (2026-10-22): Lua isn't safe to use from two threads at once. `ExecuteInGameThreadWithDelay(ms, work)` and
  `LoopInGameThreadWithDelay(ms, work)` (stopped with `CancelDelayedAction(handle)`) run on the game thread.
- **Use game objects on the game thread.** Key binds and timers may run elsewhere. `Library.keybind` and
  `Library.on_game_thread` take care of it, and `LoopInGameThreadWithDelay(ms, work)` repeats work there.
- **A hook on a Blueprint function runs after it, and gets no arguments.** UE4SS calls Lua hooks on Blueprint
  (`/Game/...`) functions once the function has run, so a hook can't change what it does, and in this game it passes
  none of the function's parameters (seen with `WBP_ItemCostInfo_C:UpdateCostInfo`, 2026-10-22: UE4SS reads the
  parameter count, which comes back as 0). Read what the function left behind instead: the widget's properties, or
  the text it wrote.
- **A uniform grid keeps a row for every row a child names, hidden or not.** To hide a tile in a
  `UniformGridPanel` (an inventory grid), collapse it and move it to row 0, column 0, or the grid keeps empty rows.
- **Functions the game's C++ calls directly can't be hooked.** Building and deconstructing never called
  the inventory functions a probe hooked. They fire only when a Blueprint or the user interface calls
  them.
- **Arrays:** an array a function returns arrives as a Lua table; an array property needs
  `Library.list`. There's no `ForEach` on a returned array.
- **`ForEach`: return `true` to stop, and nothing otherwise, never `false`.** UE4SS (3.0.1 here) takes a returned
  `false` off the stack twice, and the next element calls nil: "attempt to call a nil value" at the `ForEach` line.
  (AlchemistsVault 0.8.0's first try, 2026-10-23.)
- **No weak pointers, so no binding delegates.** A delegate's `Add` (`button.OnClicked:Add(object, "Function")`)
  makes an `FWeakObjectPtr`, whose serial number crashes the game inside UE4SS (`AllocateSerialNumber`). The same
  goes for a C++ mod making one. Read a widget's state by polling instead. (AlchemistsVault 0.8.0, 2026-10-23.)
- **A deconstructed building lingers.** Its object stays valid (`IsValid()` is true, and it's still found by
  `FindAllOf`) for about ten seconds, until the game frees it. `actor.bActorIsBeingDestroyed` is true from the
  moment it's deconstructed: check that before counting a building as placed.
- **Old worlds linger too.** After loading a save, objects from the main menu's background scene (its own
  grid subsystem, say) can still be found for a while, so `FindFirstOf` may return the wrong one. Drop cached
  objects when a map loads (`RegisterLoadMapPreHook(function() ... end)`), and prefer objects you were given
  (a hook's context) to ones you search for.
- **Keep track of objects as they appear, rather than searching for them.** `NotifyOnNewObject` on a class,
  with the real work queued for a later tick (the object isn't filled in yet), builds a list as the world
  loads; scanning every object (`ForEachUObject`) for each lookup is slow.
- **What the player is about to build:** `EnterBuildMode` fires only when a building is chosen from the build
  menu, not from the quickbar, and never for blueprints or bulk clone. The cost panel shows what the current
  choice costs whichever way it was chosen: `WBP_BuildCostPanel_C:AddIngredientData` is called with each
  item and count as it fills (seen in a third-party mod; untested here), and each `WBP_ItemCostInfo` widget
  holds one item's "have/need".
- **A key isn't a game event.** A key bind on Esc or the right mouse button runs whether or not the game
  cancels anything then. To know when building starts or ends, post-hook
  `BeltTDPlayerController:GetCurrentStateName` (`Build`, `Clone`, `Remove`, `Idle`) instead.
- **Recognise buildings by their row, not their name.** An actor's `ConfigName` is its `DT_Buildings` row; its
  object name (`BP_MetalContainer_C_2147466526`) only says which Blueprint it uses, many rows share a
  Blueprint, and names differ from machine to machine.
- **Moving items the way a player does:** `inventory:TryExchangeInventorySlot(fromComponent, fromIndex,
  toComponent, toIndex)` swaps two slots, and `MoveInventorySlot(fromComponent, index, toComponent)` moves one,
  as the inventory screen does (`inventory` is the player's, from `GetPlayerInventory()`). They act a moment
  later, not at once, so read the slots again after the next `BeltTDPlayerController:OnInventoryUpdate`.
  Untested here in multiplayer; being the screen's own actions, they're likelier to stay in step than writing
  slots directly. To read one slot, `component:GetSlotData(index, {}, {})` returns its item, with the count
  and filter in the out tables (on the player's inventory and on chests).
- **Lending items to the player's inventory runs out of room.** A mod that moves chest items into the
  inventory so the game can charge them there needs a free slot per item, and none of the player's own of
  that item to keep the two apart; a full inventory, often the very problem, stops it. Changing what the
  game charges needs a C++ mod (`docs/MODDING.md`, "C++ UE4SS mods").
- **`GetInventoryItemNum` was reported to crash a third-party mod**; `CostDisplayTest` called it constantly
  from the build tool without trouble. As with `GetCurrentStateName` above, the likely rule is: call game
  functions only once a save has loaded.
- **Multiplayer is lockstep.** Every player's game runs the same simulation from the players' commands. A
  mod that changes the game must:
  - change it on every player's machine, in the same step and order: in hooks on the simulation, never
    from a key, a timer or the user interface;
  - avoid clocks and random numbers, and iterate in a fixed order (`Library.sorted_keys`,
    `Library.by_position`), not `pairs()` order.

  Logging, and changes only the local player sees, are exempt. A change made on one machine alone (from
  a key, say) makes the players' games differ from then on.
- **Keys:** UE4SS's own keys (its `Keybinds` mod) are Ctrl + Numpad 5 to 9, Ctrl + J and Ctrl + H, for
  its dumps; Ctrl + Numpad 7, for one, dumps every loaded actor to a file. Mods here use Ctrl + Numpad 1
  to 4: probes use 2 and 3, and the example uses 1.

### Found by other mods

These were found by danipr25 for the [AF Mod Loader](https://github.com/danipr25/af-modloader) mods (game 1.1.5023
and 1.1.5024, UE4SS 1181, 2026-10-20 to 2026-10-23), and haven't been tried here.

- **Never call `QueryEnemyConfig`** (on `BeltTDUserWidget`): it crashed the game. Read item rows from the table
  instead (`Library.find_row("DT_Enemies", name)`, or `ForEachRow(function(name, row) ... end)` on the table).
  The game state's `EnemyConfig` and `EnemyCraftingConfig` are `DT_Enemies` and `DT_EnemyCrafting`. An enum read
  from a row comes back as a number (`SellType` `Liquid` is 6).
- **The game's own answers.** On `/Game/Blueprints/Core/BP_BeltTDFunctionLib.Default__BP_BeltTDFunctionLib_C`:
  - `GetItemDisplayName(FName(item), gameState, out)` puts the item's name, in the player's language, in
    `out.DisplayName`;
  - `GetEnemyIcon(FName(item), gameState, out)` puts its icon in `out.OutTexture`.

  On the game state, `GetCauldronProduct({FName(a), FName(b)}, outTime, outHeat)` returns a cauldron's product,
  with its time and heat in the out tables. It's fast: 28,900 calls took 284 ms.
- **Cauldrons:** `BP_PewterCauldron_C` (the Cauldron) and `BP_SilverCauldron_C` (the Advanced Cauldron). On each one's
  `CauldronFacility`, `FacilityContainer.Slots[i].SlotFilterName` is the filter on each slot, and
  `CachedOutputName` is the product of the last recipe. Both stay set when the cauldron is idle and empty. A hook
  on `/Script/BeltTD.CauldronFacilityComponent:NetCommandSetFilterName` fires when a player changes a slot's
  filter: the context is the cauldron's component, and the parameters are the slot (from 0) and the new filter
  (`None` when cleared).
- **A mod's `main.lua` runs before there's a world**, and `RegisterInitGameStatePostHook` fires twice when a save
  loads: once for the main menu, then once for the save. To know which world is loaded, read `WorldTitleName` on
  `FindFirstOf("GameSaveLoadSubsystem")` when its `IsPreviewScene` is false. Another slot of the same world keeps
  the title.
- **Files:** UE4SS's Lua opens files with the system's ANSI calls, so `io.open` fails on a path with letters such as
  `ñ`. `os.rename` onto a file that exists fails: remove the old file first.
- **The game has no developer console.** A mod can turn on Unreal's (as UE4SS's `ConsoleEnablerMod` does) and add
  commands with `RegisterConsoleCommandGlobalHandler`. That console also runs every engine command, some of which
  crash the game.
- **Menus:** [AF Mod Loader API](https://github.com/danipr25/af-modloader-api) adds tabs to the TAB and V menus,
  sections to an item's sheet, buttons, item tiles and lists. Its notes (`docs/tab-menu.md`,
  `docs/catalog-menu.md`, `docs/widgets.md`) say how the game builds those menus and which widget functions work
  from Lua.
