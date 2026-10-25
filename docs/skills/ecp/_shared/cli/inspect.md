# ecp inspect

Show a symbol's full context: signature, body, edges, callers, overrides, 1-hop upstream impact, contained members, and decorators.

## Usage
```bash
ecp inspect --name <SYMBOL_NAME> [--repo <PATH>]
```

## Options
- `--name X`: The name of the symbol to inspect.
- `--repo PATH`: Path to the repository (default: `.`).
- `--kind KIND`: Select the declaration of this one kind (`function`, `method`, `impl`, ...). A list is an error. `--kind impl` selects the `Impl` node that is otherwise omitted beside a struct of the same name.
- `--file PATH` (aliases `--file_path`, `--file-path`): Select the declaration in this file (equal path, then path suffix, then substring).
- `--neighbor-kind K1,K2`: Keep only the edges whose other end has one of these kinds.
- `--neighbor-file SUBSTR`: Keep only the edges whose other end is in a path that holds this text.
- `--format`: `toon` (default), `json`.

## Output fields (JSON)
- `status` — `found`, `ambiguous` or `error` (not found).
- `candidates` — on `ambiguous` only: each with `filePath`, `kind`, `line` and `target`. Run `ecp inspect <target>` to inspect one. A null `target` means no target text or filter selects it alone. An ambiguous answer has no neighbour sections: they are not evaluated, not empty.
- `symbol.decorators` — flat string list (`@` prefix stripped), e.g. `["staticmethod", "functools.cached_property"]`. Empty for undecorated nodes.
- `contained_methods` / `contained_properties` — for Class / Struct / Trait / Interface / Enum.
- `contained_variants` — for `Enum`: the EnumVariant children with their `name`, `filePath`, `line`.
- `outgoing` / `incoming` — edges keyed by RelType (`calls`, `implements`, `defines`, `decorates`, `opens_tx_scope`, `fetches`, ...). Heuristic edges land in `heuristic_outgoing` / `heuristic_incoming` with a `heuristic_note` flag.
  A `Calls` entry in `incoming` that reached a class through its constructor carries `viaConstructor` (the constructor's name); only `Calls` edges are read through a constructor.

## Best For
- Understanding a function's implementation without leaving the terminal.
- Seeing what methods, properties, **or enum variants** a type declares.
- Checking 1-hop callers quickly.
- Reading the decorator / annotation list applied to a symbol.
