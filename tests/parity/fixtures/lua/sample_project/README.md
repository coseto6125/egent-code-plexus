# Lua Sample Project

This fixture exercises the `LuaProvider` for `cgn-analyzer`.

## Files

- `init.lua` — entry point; calls `require("module_a")`, defines `greet` and `run` functions
- `module_a.lua` — defines `Animal` (PascalCase table-as-class with `new` and `speak`), `M.log` (module method), and `internal_helper` (local function)

## Expected symbols

| Name | Kind | File |
|---|---|---|
| `greet` | Function | init.lua |
| `run` | Function | init.lua |
| `Animal` | Class | module_a.lua |
| `new` | Function | module_a.lua |
| `speak` | Function | module_a.lua |
| `log` | Function | module_a.lua |
| `internal_helper` | Function | module_a.lua |
| `M` | Const | module_a.lua |

## Import coverage

`init.lua` does `require("module_a")` which should appear as an `@import` edge.
