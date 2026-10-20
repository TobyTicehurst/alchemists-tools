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

1. Install UE4SS (v3.0.1 is the version tested) into `AlchemyFactory/Binaries/Win64/`.
2. Start the game once.
3. Check the log, `Binaries/Win64/ue4ss/UE4SS.log`: everything a mod prints goes there.

The tools need UE4SS anyway, for the game's `.usmap` (Ctrl + Numpad 6 in a new save).

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
- **Use game objects on the game thread.** Key binds and timers may run elsewhere. `Library.keybind` and
  `Library.on_game_thread` take care of it, and `LoopInGameThreadWithDelay(ms, work)` repeats work there.
- **Functions the game's C++ calls directly can't be hooked.** Building and deconstructing never called
  the inventory functions a probe hooked. They fire only when a Blueprint or the user interface calls
  them.
- **Arrays:** an array a function returns arrives as a Lua table; an array property needs
  `Library.list`. There's no `ForEach` on a returned array.
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
