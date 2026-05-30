# SQL-in-string → `code → table` edges (the dbgraph "D" integration)

**Date:** 2026-05-30
**Status:** Design approved (continuous-dev), pending plan

## Problem

ecp's graph drops a real edge: when application code issues raw SQL as a string
literal — `pool.execute("SELECT ... FROM channels ...")` — the table reference
lives *inside* the string, which the AST treats as an opaque `string_literal`.
So "which functions read/write the `channels` table" is unanswerable by
`ecp impact`, even though the table itself is already a graph node (SQL DDL
files emit each table as `NodeKind::Class`, see `sql/spec.rs:12`).

A standalone POC (`~/dbgraph`) validated the fix end-to-end against enoract's
live schema + polyglot code: 19 tables, 724 `code→table` edges, recall matching
an exhaustive grep, precision *better* than grep (sqlglot resolves JOINs grep
misses). The POC connected to a live DB and ran Python `sqlglot` at query time.
This spec brings that capability **into ecp**, where it belongs and runs faster.

### Why this passes the LLM-utility gate

**Gate (A) — Graph completeness.** Without these edges, `ecp impact channels`
omits every code caller that touches the table via raw SQL → an LLM doing a
schema migration misses callers and ships a breaking change. This is the exact
"function-pointer / vtable dispatch" class of gap the gate names.

No new `NodeKind` is required: the table is already a `Class` node. We add one
`RelType` (`QueriesTable`) carrying read/write semantics in `Edge.reason`,
mirroring how `UsesPathLiteral` encodes `sink:read` / `sink:write`.

## Why "D" beats the alternatives (performance)

| | dbgraph POC (Python) | "C" (ecp nodes + Python sqlglot) | **"D" (this spec, all-Rust)** |
|---|---|---|---|
| string extraction | ast + regex | tree-sitter (Rust) | tree-sitter (Rust) |
| SQL parse | Python sqlglot | Python sqlglot, **query-time** | **tree-sitter-sequel (Rust), index-time** |
| new dependency | — | sqlglot | **none** (reuse `tree-sitter-sequel 0.3.11`, already a dep) |
| query latency | ~100ms+ (Python start) | ~100ms+ | **<30ms** (edge already in graph) |
| cross-process | yes | yes | **no** |

D resolves SQL once at index time, in Rust, storing the edge in the rkyv graph —
satisfying ecp's non-negotiable per-query <30ms. C leaves SQL parsing in Python
at query time, violating it.

## Non-Goals

- No live-DB introspection. ecp stays purely static. The schema truth comes from
  the `.sql` migration files already in the repo (already indexed: `ecp find
  channels` resolves to a migration file). No `pg_dump`, no DB connection.
- No new `NodeKind`. Tables are `Class` nodes already.
- Dynamic SQL whose **table name** is interpolated (`f"FROM {tbl}"`) stays a
  `BlindSpot`, never a fabricated edge.

## Architecture

Mirror the established `PathLiteral` pipeline (per-lang capture → language-neutral
predicate → sink classification → edge with `reason`). The new module is the SQL
analogue of `path_literal.rs`.

```
Per-lang parser (existing string_literal captures, 14 langs)
        │  raw literal value (quotes/sigils stripped)
        ▼
sql_literal::is_sql_shaped(&str)        ← language-neutral predicate (new)
        │  keep only SQL-looking strings (SELECT/INSERT/UPDATE/DELETE/… + table pos)
        ▼
sql_literal::parse_tables(&str, dialect) ← tree-sitter-sequel (existing dep)
        │  → [(table, verb)]  verb ∈ {read, write}; unresolved → BlindSpot
        ▼
post_process: resolve table name → existing Class node (from SQL DDL)
        │  emit  Function ──QueriesTable{reason:"read"|"write"}──▶ Class(table)
        ▼
ecp impact channels --direction upstream  → all code callers, free
```

### New pieces

1. **`crates/ecp-analyzer/src/sql_literal/`** — language-neutral, `&str`-only:
   - `is_sql_shaped(s: &str) -> bool` — cheap pre-filter (a SQL verb keyword
     followed by a plausible table position). Rejects log lines, prose.
   - `parse_tables(s: &str) -> SqlTables` — run `tree-sitter-sequel` over the
     literal; collect referenced tables + classify the statement verb
     (SELECT→read, INSERT/UPDATE/DELETE→write). Interpolation placeholders
     (`{__ph__}`-style, language-specific) handled as in the POC: a static
     table still resolves; a dynamic table name yields `unresolved`.
   - Mirrors `path_literal.rs`: takes only `&str`, so per-language drift is
     impossible. Escape/interp rules unified here.

2. **`RawSqlRef`** in `analyzer::types` — per-file carrier
   `{ symbol_name, tables: [(name, verb)], unresolved: bool, span }`, populated
   by each language parser from its `string_literal` captures (same hook point
   that feeds `path_literals`).

3. **`crates/ecp-analyzer/src/post_process/sql_table_edges.rs`** — promote
   `RawSqlRef`s to edges: resolve each table name against the `Class` nodes the
   SQL DDL parser emitted (SymbolTable lookup, same pattern as
   `schema_field_mirrors::emit_edges`). A table not found among known tables is
   dropped (false hit); an unresolved ref becomes a `BlindSpot`. Emit
   `Function ──QueriesTable──▶ Class` with `reason = "read" | "write"`.

4. **`RelType::QueriesTable`** — appended at the END of the enum (rkyv
   discriminant stability, per CLAUDE.md schema rule). Doc comment names the
   LLM-query benefit: "code symbol → DB table it reads/writes via raw SQL;
   `ecp impact <table> --upstream` returns schema-migration blast radius."

### Reuse, not reinvent

- `tree-sitter-sequel` — already parses `.sql` DDL; reused to parse SQL strings.
- The `string_literal` capture hook — already feeds `path_literals`; `RawSqlRef`
  rides the same per-file capture pass (no new tree-sitter query traversal).
- `schema_field_mirrors::emit_edges` — the table-name → node resolution +
  SymbolTable-miss-drops-silently pattern is copied for `sql_table_edges`.
- `BlindSpot` — the existing honest-no-data channel carries unresolved dynamic
  SQL.

## 14-language coverage

Per CLAUDE.md, any parser/core change ships tests for all 14 mainstream langs.
The raw-SQL-string idiom differs per language; the POC already proved the
string-extraction matrix (Go backtick, Java/Rust/C#/PHP/Ruby/JS/Kotlin quotes).
Tests: one `<lang>_sql_table_edges.rs` per language asserting
`fn list_channels(){ db.query("SELECT id FROM channels") }` yields a
`QueriesTable(read)` edge from the function to the `channels` Class node, plus
the write verbs and the dynamic-table `BlindSpot` case.

The language-neutral `is_sql_shaped` / `parse_tables` are unit-tested directly
(string-in → tables-out), independent of any one language's parser.

## Performance non-negotiables

- `is_sql_shaped` is the hot pre-filter: a byte scan for a leading SQL keyword
  before any tree-sitter work, so non-SQL literals cost ~nothing (mirrors
  `is_path_shaped`).
- `tree-sitter-sequel` parse runs only on literals that pass the pre-filter, at
  index time, never on `ecp` query hot paths.
- Edge resolution is one SymbolTable lookup per table name — O(refs), offline.

## Testing strategy

- **Unit (language-neutral):** `is_sql_shaped` accept/reject table; `parse_tables`
  on SELECT/INSERT/UPDATE/DELETE/JOIN/CTE → expected (table, verb) sets;
  dynamic-table string → unresolved.
- **Per-language (14):** the `<lang>_sql_table_edges.rs` matrix above.
- **Integration:** reindex a fixture repo with a `.sql` migration (table = Class
  node) + code querying it; assert `ecp impact <table> --upstream` lists the
  code function, with read/write reason.
- **BlindSpot:** dynamic table name → assert a `BlindSpot` record, no edge.
- **No-regression:** existing SQL DDL tests + `path_literal` tests stay green
  (the new capture path must not perturb `PathLiteral` emission).

## Open questions (deferred to plan)

- Per-language interpolation-placeholder normalization: the POC's `__ph__`
  substitution is Python-f-string-aware; Go/JS template literals and Rust
  `format!` need their own `{…}` / `${…}` recognition in `sql_literal`.
- Column-level edges (`QueriesTable` to a specific `SchemaField`) vs table-level
  only — start table-level (matches POC's confident path); column attribution
  is a follow-up once SQL DDL emits per-column `SchemaField` nodes.
- Whether `is_sql_shaped` should require a recognized table position or just a
  SQL verb keyword (precision vs recall tuning — POC used verb keyword + parse).
