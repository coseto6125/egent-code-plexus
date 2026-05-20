# Zig Sample Project Fixture

This fixture exercises the ZigProvider parser for ecp-analyzer.

## Files

- `main.zig` — entrypoint with `pub fn main()` and `pub fn runTests()`, imports `utils.zig` via `@import("utils.zig")`
- `utils.zig` — defines `Point` struct with `init` and `distance` methods, `add` and `greet` free functions, and a `MAX_VALUE` const

## Expected symbols

| Symbol      | Kind     | File       |
|-------------|----------|------------|
| main        | Function | main.zig   |
| runTests    | Function | main.zig   |
| Point       | Class    | utils.zig  |
| init        | Function | utils.zig  |
| distance    | Function | utils.zig  |
| add         | Function | utils.zig  |
| greet       | Function | utils.zig  |
| MAX_VALUE   | Const    | utils.zig  |

## Import edges

- `main.zig` → `utils.zig` (via `@import("utils.zig")`)
- `main.zig` → `std` (via `@import("std")`)
- `utils.zig` → `std` (via `@import("std")`)
