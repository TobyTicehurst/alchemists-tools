-- AlchemistsLibrary: shared helpers for Alchemy Factory UE4SS mods.
--
--   local Library = require("AlchemistsLibrary")
--   local log = Library.logger("MyMod")
--   log.log("hello")                                   -- ue4ss/UE4SS.log: [MyMod] hello
--
-- A mod that uses it lists "AlchemistsLibrary" in its mod.json dependencies; installing puts this file in
-- ue4ss/Mods/shared/, where UE4SS's require finds it.
--
-- Multiplayer is lockstep: every machine runs the same simulation from the players'
-- commands. A mod that changes game state must change it on every machine, in the same simulation step, in the
-- same order:
--   * change state only in hooks on the simulation (building, deconstructing, commands being applied), never from
--     a timer, a key, or the UI;
--   * be deterministic: no clocks, no random numbers, and no pairs() order (use Library.sorted_keys and
--     Library.by_position);
--   * let player actions go through the game's own commands, which reach every machine.
-- Logging, and changes that only the local player sees, are exempt.
--
-- Marked "untested" below: calls not yet tried in this game.

local Library = {version = "0.2.3"}

-- Logging -------------------------------------------------------------------------------------------------

-- A logger for one mod: log(message), once(message) (logs each distinct message only once, for problems that
-- would otherwise repeat every tick), and error(message).
function Library.logger(tag)
    local prefix = "[" .. tag .. "] "
    local seen = {}
    local logger = {}
    function logger.log(message) print(prefix .. tostring(message) .. "\n") end
    function logger.once(message)
        message = tostring(message)
        if seen[message] then return end
        seen[message] = true
        logger.log(message)
    end
    function logger.error(message) logger.log("error: " .. tostring(message)) end
    return logger
end

-- Runs work(...) and logs an error instead of stopping the mod. Returns whether it worked, then work's results.
function Library.safely(logger, work, ...)
    local results = table.pack(pcall(work, ...))
    if not results[1] then logger.once("error: " .. tostring(results[2])) end
    return table.unpack(results, 1, results.n)
end

-- Runs work on the game thread, where game objects may be used (key binds and timers may run elsewhere).
function Library.on_game_thread(logger, work)
    ExecuteInGameThread(function() Library.safely(logger, work) end)
end

-- A key bind whose work runs on the game thread, e.g.
--   Library.keybind(Key.NUM_ONE, {ModifierKey.CONTROL}, log, function() ... end)
-- Keys are local to one player: don't change shared game state from one (see the lockstep rules above).
function Library.keybind(key, modifiers, logger, work)
    RegisterKeyBind(key, modifiers, function() Library.on_game_thread(logger, work) end)
end

-- Values --------------------------------------------------------------------------------------------------

-- The elements of a TArray, as a Lua list.
function Library.list(array)
    -- UE4SS gives an array a function returns (e.g. K2_GetComponentsByClass) as a Lua table already
    if type(array) == "table" then return array end
    local items = {}
    array:ForEach(function(_, element) items[#items + 1] = element:get() end)
    return items
end

local function plain_text(value)
    if type(value) ~= "userdata" then return tostring(value) end
    local ok, text = pcall(function() return value:ToString() end)  -- FName, FString, FText
    if ok and type(text) == "string" then return '"' .. text .. '"' end
    ok, text = pcall(function()
        if not value:IsValid() then return "(invalid object)" end
        return value:GetFullName()
    end)
    if ok and type(text) == "string" then return text end
    return tostring(value)
end

-- A readable form of any value, for logs. layout describes what a struct or array holds (UE4SS can't list a
-- struct's fields itself): {fields = {{name = "IngredientName"}, {name = "Count"}}}, {array = <layout>}, or nil.
-- alchemists-tools probe writes layouts from the .usmap.
function Library.describe(value, layout, depth)
    depth = depth or 0
    if type(value) == "userdata" and layout and depth < 4 then
        if layout.array then
            local ok, items = pcall(Library.list, value)
            if not ok then return "(array: " .. tostring(items) .. ")" end
            local parts = {}
            for i, item in ipairs(items) do
                if i > 16 then
                    parts[#parts + 1] = "..."
                    break
                end
                parts[#parts + 1] = Library.describe(item, layout.array, depth + 1)
            end
            return "[" .. #items .. "] {" .. table.concat(parts, ", ") .. "}"
        end
        if layout.fields then
            local parts = {}
            for _, field in ipairs(layout.fields) do
                local ok, field_value = pcall(function() return value[field.name] end)
                parts[#parts + 1] = field.name .. " = "
                    .. (ok and Library.describe(field_value, field.layout, depth + 1) or "?")
            end
            return "{" .. table.concat(parts, ", ") .. "}"
        end
    end
    return plain_text(value)
end

-- Lockstep helpers ----------------------------------------------------------------------------------------

-- A table's keys in sorted order, for iterating the same way on every machine (pairs() order differs).
function Library.sorted_keys(t)
    local keys = {}
    for key in pairs(t) do keys[#keys + 1] = key end
    table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
    return keys
end

-- Actors sorted by their position (X, then Y, then Z, then name): the same order on every machine. Untested.
function Library.by_position(actors)
    local keyed = {}
    for _, actor in ipairs(actors) do
        local location = actor:K2_GetActorLocation()
        keyed[#keyed + 1] = {actor = actor, x = location.X, y = location.Y, z = location.Z,
                             name = actor:GetFName():ToString()}
    end
    table.sort(keyed, function(a, b)
        if a.x ~= b.x then return a.x < b.x end
        if a.y ~= b.y then return a.y < b.y end
        if a.z ~= b.z then return a.z < b.z end
        return a.name < b.name
    end)
    local sorted = {}
    for i, entry in ipairs(keyed) do sorted[i] = entry.actor end
    return sorted
end

-- Whether this machine has authority over the actor (the host, in a multiplayer game), or nil if unknown.
-- Untested; with lockstep, every machine may run the same code anyway.
function Library.has_authority(actor)
    local ok, result = pcall(function() return actor:HasAuthority() end)
    if ok then return result end
    return nil
end

-- The game's objects --------------------------------------------------------------------------------------

-- Objects of a class that exist in the world: not the class default (Default__) or Blueprint component
-- templates (_GEN_VARIABLE). E.g. Library.placed("CraftFacilityComponent").
function Library.placed(class_name)
    local found = {}
    for _, object in ipairs(FindAllOf(class_name) or {}) do
        if object:IsValid() then
            local full_name = object:GetFullName()
            if not full_name:find("Default__", 1, true) and not full_name:find("GEN_VARIABLE", 1, true) then
                found[#found + 1] = object
            end
        end
    end
    return found
end

-- The DT_Buildings row of a placed building (an actor, or one of its components), or nil.
function Library.building_row(object)
    local ok, row = pcall(function()
        local actor = object
        local has_owner, owner = pcall(function() return object:GetOwner() end)
        if has_owner and owner and owner:IsValid() then actor = owner end
        return actor.ConfigName:ToString()
    end)
    if ok then return row end
    return nil
end

-- An actor's own components of a class (a /Script/ path), through the engine's
-- K2_GetComponentsByClass. Untested.
function Library.components(actor, class_path, report)
    report = report or function() end
    local class = StaticFindObject(class_path)
    if not class or not class:IsValid() then
        report("no class " .. class_path)
        return {}
    end
    local ok, found = pcall(function() return Library.list(actor:K2_GetComponentsByClass(class)) end)
    if not ok then
        report("K2_GetComponentsByClass failed: " .. tostring(found))
        return {}
    end
    local classes = {}
    for _, component in ipairs(found) do classes[#classes + 1] = Library.describe(component) end
    report(string.format("%d %s component(s): %s", #found, class_path, table.concat(classes, "; ")))
    return found
end

-- The items an actor holds, as {{name = "WoodBoard", count = 12}, ...}: the slots of its
-- BuildingFacilityComponents (FacilityContainer: chests, bags, machines) and BeltTDInventoryComponents
-- (ContainerInventory: the player). Empty slots are left out. Untested.
--
-- Call it once the world has loaded (e.g. from a key). Version 0.2.0 found the components by scanning
-- every component in the world, during the world's load, and that crashed the game inside UE4SS.
--
-- report, if given, is called with a line for each step (which components, how many slots), to find out
-- why something holds "nothing".
function Library.contents(actor, report)
    report = report or function() end
    local items = {}
    local function read(component, property)
        local ok, slots = pcall(function() return Library.list(component[property].Slots) end)
        if not ok then
            report("   can't read " .. property .. ": " .. tostring(slots))
            return
        end
        report(string.format("   %s: %d slot(s)%s", property, #slots,
            #slots > 0 and (", the first " .. Library.describe(slots[1],
                {fields = {{name = "ItemName"}, {name = "ItemStack"}, {name = "SlotFilterName"}}})) or ""))
        for _, slot in ipairs(slots) do
            local name = slot.ItemName:ToString()
            if name ~= "None" then items[#items + 1] = {name = name, count = slot.ItemStack} end
        end
    end
    local found = 0
    for _, spec in ipairs({{"/Script/BeltTD.BuildingFacilityComponent", "FacilityContainer"},
                           {"/Script/BeltTD.BeltTDInventoryComponent", "ContainerInventory"}}) do
        for _, component in ipairs(Library.components(actor, spec[1], report)) do
            found = found + 1
            read(component, spec[2])
        end
    end
    if found == 0 then
        -- A Blueprint's components are also its variables, named as in the Blueprint
        for _, name in ipairs({"IngredientContainer"}) do
            local ok, component = pcall(function() return actor[name] end)
            if ok and component and component:IsValid() then
                report("   found " .. name .. " as a variable: " .. Library.describe(component))
                read(component, "FacilityContainer")
            else
                report("   no variable " .. name)
            end
        end
    end
    return items
end

-- A data table, e.g. Library.data_table("DT_Buildings"), or nil before the game has loaded it.
function Library.data_table(name)
    local found = StaticFindObject("/Game/DataTables/" .. name .. "." .. name)
    if found and found:IsValid() then return found end
    return nil
end

-- A row of a data table, e.g. Library.find_row("DT_Buildings", "Processor"), or nil.
function Library.find_row(table_name, row)
    local found = Library.data_table(table_name)
    if not found then return nil end
    return found:FindRow(row)
end

return Library
