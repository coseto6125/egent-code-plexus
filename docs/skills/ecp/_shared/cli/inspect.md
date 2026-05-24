# ecp inspect

Show a symbol's full context: signature, body, edges, callers, overrides, 1-hop upstream impact, contained members, and decorators.

## Usage
```bash
ecp inspect --name <SYMBOL_NAME> [--repo <PATH>]
```

## Options
- `--name X`: The name of the symbol to inspect.
- `--repo PATH`: Path to the repository (default: `.`).
- `--kind KIND`: Filter target side of edges by kind.
- `--file_path SUBSTR`: Filter target side of edges by file path substring.
- `--format`: `toon` (default), `json`.

## Output fields (JSON)
- `symbol.decorators` — flat string list (`@` prefix stripped), e.g. `["staticmethod", "functools.cached_property"]`. Empty for undecorated nodes.
- `contained_methods` / `contained_properties` — for Class / Struct / Trait / Interface / Enum.
- `contained_variants` — for `Enum`: the EnumVariant children with their `name`, `filePath`, `line`.
- `outgoing` / `incoming` — edges keyed by RelType (`calls`, `implements`, `defines`, `decorates`, `opens_tx_scope`, `fetches`, ...). Heuristic edges land in `heuristic_outgoing` / `heuristic_incoming` with a `heuristic_note` flag.

## Best For
- Understanding a function's implementation without leaving the terminal.
- Seeing what methods, properties, **or enum variants** a type declares.
- Checking 1-hop callers quickly.
- Reading the decorator / annotation list applied to a symbol.
