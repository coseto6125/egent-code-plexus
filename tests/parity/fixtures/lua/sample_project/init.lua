-- init.lua: Entry point for the sample Lua project.
-- Imports module_a and calls its functions.

local M = require("module_a")

local function greet(name)
    return "Hello, " .. name .. "!"
end

local function run()
    local result = greet("World")
    print(result)
    M.log("init")
    local obj = M.Animal.new("Cat")
    obj:speak()
end

run()
