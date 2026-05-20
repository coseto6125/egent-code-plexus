# ecp inspect

Show a symbol's full context: signature, body, edges, callers, overrides, and 1-hop upstream impact.

## Usage
```bash
ecp inspect --name <SYMBOL_NAME> [--repo <PATH>]
```

## Options
- `--name X`: The name of the symbol to inspect.
- `--repo PATH`: Path to the repository (default: `.`).
- `--format`: `toon` (default), `json`.

## Best For
- Understanding a function's implementation without leaving the terminal.
- Seeing exactly where a class is defined and what methods it contains.
- Checking 1-hop callers quickly.
