-- ExampleBagReporter: the worked example in docs/examples/lua.md.
--
-- Deconstructing buildings puts their contents into a bag (BP_PhantomTemporaryBag). This mod logs each new
-- bag, and on Ctrl + Numpad 1 lists what the newest one holds. It only reads, so it's safe in multiplayer.
-- Output goes to ue4ss/UE4SS.log, in lines starting with [ExampleBagReporter].

local Library = require("AlchemistsLibrary")
local log = Library.logger("ExampleBagReporter")

local BAG = "/Game/Blueprints/Buildings/BP_PhantomTemporaryBag.BP_PhantomTemporaryBag_C"
local newest = nil

-- Runs as each bag is made, including the bags already in a save while it loads. Only note the bag here:
-- reading the world's objects while a world loads can crash the game (docs/examples/lua.md, "Rules").
NotifyOnNewObject(BAG, function(bag)
    Library.safely(log, function()
        newest = bag
        log.log("a bag appeared: " .. Library.describe(bag))
    end)
end)

-- Read the bag only when asked, once the world has loaded. The key's work runs on the game thread.
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

log.log("loaded: press Ctrl + Numpad 1 after deconstructing something")
