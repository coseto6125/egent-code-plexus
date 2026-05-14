-- module_a.lua: Defines a table-as-class (Animal) with methods, and utility functions.

local M = {}

-- PascalCase table → treated as a class by the parser
local Animal = {}
Animal.__index = Animal

function Animal.new(name)
    local self = setmetatable({}, Animal)
    self.name = name
    return self
end

function Animal:speak()
    print(self.name .. " makes a sound.")
end

-- Utility function exposed on the module
function M.log(msg)
    print("[LOG] " .. msg)
end

local function internal_helper(x)
    return x * 2
end

M.Animal = Animal
M.double = internal_helper

return M
