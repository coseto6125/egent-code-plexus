# Crystal Sample Fixture

Two-file Crystal project demonstrating classes, modules, inheritance, methods with type annotations, and require imports.

- `helper.cr` — defines `Greetable` module and `Helper` class with a top-level `VERSION` constant
- `app.cr` — defines `App < Helper` (inherits Helper), uses `require "./helper"`, APP_NAME constant, and methods with return type annotations (`run : Int32`, `start : App`)

Expected symbols: `App`, `Helper`, `Greetable`, `run`, `start`, `initialize`, `format_message`, `greet`, `APP_NAME`, `VERSION`
