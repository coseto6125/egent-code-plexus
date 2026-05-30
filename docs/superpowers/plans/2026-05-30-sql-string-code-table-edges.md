# SQL-in-string → code→table edges Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ecp impact <table> --direction upstream` return the code functions that read/write that table via raw SQL strings, by parsing SQL string literals at index time and emitting `Function ──QueriesTable{read|write}──▶ Class(table)` edges.

**Architecture:** Mirror the existing `PathLiteral` pipeline. Each per-language parser already captures `string_literal` nodes; we run a language-neutral `sql_literal::{is_sql_shaped, parse_tables}` (reusing the in-tree `tree-sitter-sequel` dep) over those literals to build `RawSqlRef`s, then a `post_process::sql_table_edges` pass resolves each table name to its existing `Class` node and emits a new `QueriesTable` edge. No new `NodeKind`; no new dependency; no live DB.

**Tech Stack:** Rust, `tree-sitter-sequel 0.3.11` (already a workspace dep), rkyv graph, `phf`. Build: `cargo build -p ecp-analyzer`. Test: `cargo test -p ecp-analyzer`.

---

## Project Conventions (from CLAUDE.md)

- **rkyv discriminants are append-only** — new `NodeKind`/`RelType` variants go at the END of the enum.
- **WHY-only comments** — no comments explaining WHAT; identifiers self-describe.
- **Surgical** — every line traces to the task. No drive-by fixes.
- **No attribution trailers** — commit messages carry no `Co-Authored-By` / "Generated with Claude Code".
- **Format touched files only**: `rustfmt --edition 2021 <file>` (avoid `cargo fmt -p` blast radius).
- Tests: `cargo test -p ecp-analyzer <test_name>`.

## Coverage strategy (14-language rule)

The spec requires 14-language coverage. The language-bound part is ONLY "which
string-literal captures feed `sql_refs`" — the SQL parsing + edge emission is
language-neutral. So:

- **Tasks 1-7** build and prove the neutral core + the wiring on **3
  representative languages**: Python (ast-precise), Go (backtick raw strings),
  Rust (`sqlx::query!` macro). These three exercise the distinct string-literal
  shapes.
- **Task 8** is the mechanical fan-out: replicate the per-language hook + test
  for the remaining 11 mainstream langs (TypeScript, JavaScript, Java, Kotlin,
  C#, PHP, Ruby, Swift, C, C++, Dart). Each is a copy of the Go/Rust pattern
  with that language's `string_literal` capture.

This keeps the plan reviewable while honouring the coverage rule.

---

## Task 1: Add `RelType::QueriesTable`

**Files:**
- Modify: `crates/ecp-core/src/graph.rs` (append to `RelType` enum after `CompensatedBy,` at line ~522)
- Test: `crates/ecp-core/src/graph.rs` (inline `#[cfg(test)]` or rely on build)

- [ ] **Step 1: Write the failing test**

Add to the existing test module in `crates/ecp-core/src/graph.rs` (find `#[cfg(test)] mod tests`), or create one. Test:

```rust
#[test]
fn queries_table_reltype_roundtrips() {
    // QueriesTable must survive the rkyv archived round-trip like every other
    // RelType — proves the discriminant is wired into the archive derive.
    let rt = RelType::QueriesTable;
    let bytes = rkyv::to_bytes::<_, 256>(&rt).expect("serialize");
    let archived = unsafe { rkyv::archived_root::<RelType>(&bytes) };
    let back: RelType = archived.deserialize(&mut rkyv::Infallible).expect("deserialize");
    assert_eq!(back, RelType::QueriesTable);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ecp-core queries_table_reltype_roundtrips`
Expected: FAIL to compile — `no variant named QueriesTable`.

- [ ] **Step 3: Add the variant**

In `crates/ecp-core/src/graph.rs`, locate the END of the `RelType` enum (the last variant is `CompensatedBy,` near line 522). Append AFTER it, still inside the enum:

```rust
    /// `Function` / `Method` → a database `Class` (table) it reads or writes
    /// via a raw SQL string literal. `Edge.reason` carries `"read"` (SELECT) or
    /// `"write"` (INSERT/UPDATE/DELETE). LLM-utility (A) Graph completeness:
    /// without this edge `ecp impact <table> --upstream` omits every caller
    /// that touches the table through raw SQL, so a schema-migration query
    /// silently misses breaking callers. Appended at the END for rkyv
    /// discriminant stability.
    QueriesTable,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ecp-core queries_table_reltype_roundtrips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt --edition 2021 crates/ecp-core/src/graph.rs
git add crates/ecp-core/src/graph.rs
git commit -m "feat(graph): add QueriesTable RelType for code→table SQL edges"
```

---

## Task 2: Add `RawSqlRef` type + `LocalGraph.sql_refs` field

**Files:**
- Modify: `crates/ecp-core/src/analyzer/types.rs` (add `RawSqlRef` near `RawPathLiteral` at line ~115; add field to `LocalGraph` near `path_literals` at line ~571)
- Test: `crates/ecp-core/src/analyzer/types.rs` inline test

- [ ] **Step 1: Write the failing test**

Add to a `#[cfg(test)]` module in `crates/ecp-core/src/analyzer/types.rs`:

```rust
#[test]
fn raw_sql_ref_carries_tables_and_verb() {
    let r = RawSqlRef {
        tables: vec![("channels".to_string(), SqlVerb::Read)],
        unresolved: false,
        span: (1, 0, 1, 40),
        enclosing_symbol: Some("list_channels".to_string()),
        enclosing_owner: None,
    };
    assert_eq!(r.tables[0].0, "channels");
    assert_eq!(r.tables[0].1, SqlVerb::Read);
    assert!(!r.unresolved);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ecp-core raw_sql_ref_carries_tables_and_verb`
Expected: FAIL — `RawSqlRef` / `SqlVerb` not found.

- [ ] **Step 3: Add the types and field**

In `crates/ecp-core/src/analyzer/types.rs`, near `RawPathLiteral` (line ~115), add:

```rust
/// Read vs write classification of a SQL statement, derived from its leading
/// verb. Carried into `Edge.reason` so `ecp impact` can distinguish readers
/// from writers of a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlVerb {
    Read,
    Write,
}

impl SqlVerb {
    pub fn as_reason(self) -> &'static str {
        match self {
            SqlVerb::Read => "read",
            SqlVerb::Write => "write",
        }
    }
}

/// A raw-SQL string literal in application code, resolved to the tables it
/// references. Emitted by per-language parsers (same `string_literal` capture
/// hook that feeds `path_literals`) and promoted to `QueriesTable` edges by
/// `post_process::sql_table_edges`.
pub struct RawSqlRef {
    /// Tables referenced by the statement, each with its access verb. Empty
    /// when `unresolved`.
    pub tables: Vec<(String, SqlVerb)>,
    /// True when the SQL could not be statically resolved (interpolated table
    /// name, parse failure) — becomes a `BlindSpot`, never a fabricated edge.
    pub unresolved: bool,
    /// Span of the literal in source.
    pub span: (u32, u32, u32, u32),
    /// Enclosing Function/Method name; `None` for module-top-level literals.
    pub enclosing_symbol: Option<String>,
    /// Owner class when the enclosing symbol is a method.
    pub enclosing_owner: Option<String>,
}
```

Then in `pub struct LocalGraph` (line ~548), AFTER the `path_literals` field (line ~571), add a field:

```rust
    /// Raw-SQL string references captured by per-language parsers; promoted to
    /// `QueriesTable` edges in post-process. `None` when the file has none.
    pub sql_refs: Option<Box<[RawSqlRef]>>,
```

- [ ] **Step 4: Fix all `LocalGraph` construction sites**

Adding a field breaks every `LocalGraph { … }` literal. Find them:

Run: `grep -rln "LocalGraph {" crates/ecp-analyzer/src crates/ecp-core/src`

For EACH match, add `sql_refs: None,` alongside the existing `path_literals: …,` line. (There are ~30 per-language parsers plus test fixtures. Each is a one-line addition.)

- [ ] **Step 5: Run test + build to verify**

Run: `cargo test -p ecp-core raw_sql_ref_carries_tables_and_verb && cargo build -p ecp-analyzer`
Expected: test PASS, analyzer builds (all construction sites fixed).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/ecp-core/src/analyzer/types.rs
git add crates/ecp-core/src/analyzer/types.rs crates/ecp-analyzer/src
git commit -m "feat(types): add RawSqlRef + LocalGraph.sql_refs field"
```

---

## Task 3: Language-neutral `sql_literal` core — `is_sql_shaped` + `parse_tables`

**Files:**
- Create: `crates/ecp-analyzer/src/sql_literal/mod.rs`
- Modify: `crates/ecp-analyzer/src/lib.rs` (add `pub mod sql_literal;`)
- Test: `crates/ecp-analyzer/tests/sql_literal_unit.rs`

This is the load-bearing logic — `&str` in, tables+verb out. tree-sitter-sequel
node structure (verified via probe):
- table refs are `object_reference` with a `name:` field child, reachable under
  `from`/`relation`/`insert`/`update`/`delete` — but NOT under `field` (which is
  a column qualifier like `c.x`).
- statement kind node (`select`/`insert`/`update`/`delete`) gives the verb.

- [ ] **Step 1: Write the failing test**

`crates/ecp-analyzer/tests/sql_literal_unit.rs`:
```rust
use ecp_analyzer::sql_literal::{is_sql_shaped, parse_tables};
use ecp_core::analyzer::types::SqlVerb;

#[test]
fn is_sql_shaped_accepts_select_rejects_prose() {
    assert!(is_sql_shaped("SELECT id FROM channels WHERE org_id = $1"));
    assert!(is_sql_shaped("INSERT INTO channels (a) VALUES ($1)"));
    assert!(!is_sql_shaped("syncing channels for org"));
    assert!(!is_sql_shaped("user logged in successfully"));
    assert!(!is_sql_shaped(""));
}

#[test]
fn parse_tables_select_is_read() {
    let r = parse_tables("SELECT id, slug FROM channels WHERE org_id = $1");
    assert!(!r.unresolved);
    assert_eq!(r.tables, vec![("channels".to_string(), SqlVerb::Read)]);
}

#[test]
fn parse_tables_insert_update_delete_are_write() {
    for sql in [
        "INSERT INTO channels (slug) VALUES ($1)",
        "UPDATE channels SET slug = $1 WHERE id = $2",
        "DELETE FROM channels WHERE id = $1",
    ] {
        let r = parse_tables(sql);
        assert!(!r.unresolved, "sql={sql}");
        assert_eq!(r.tables, vec![("channels".to_string(), SqlVerb::Write)], "sql={sql}");
    }
}

#[test]
fn parse_tables_join_collects_both_tables_not_column_qualifiers() {
    // `c.x` / `b.y` are column qualifiers (field object_references), NOT tables.
    let r = parse_tables("SELECT a FROM channels c JOIN bots b ON c.x = b.y");
    assert!(!r.unresolved);
    let names: Vec<&str> = r.tables.iter().map(|(t, _)| t.as_str()).collect();
    assert!(names.contains(&"channels"));
    assert!(names.contains(&"bots"));
    assert!(!names.contains(&"c") && !names.contains(&"b"));
}

#[test]
fn parse_tables_unparseable_is_unresolved() {
    let r = parse_tables("this is not sql at all FROM");
    assert!(r.unresolved);
    assert!(r.tables.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ecp-analyzer --test sql_literal_unit`
Expected: FAIL — `sql_literal` module not found.

- [ ] **Step 3: Implement `crates/ecp-analyzer/src/sql_literal/mod.rs`**

```rust
//! Language-neutral SQL-string analysis: decide whether a string literal is a
//! SQL statement and, if so, extract the tables it references with a read/write
//! verb. Takes only `&str`, so per-language drift is impossible (mirrors
//! `path_literal`). Reuses the in-tree `tree-sitter-sequel` grammar.

use ecp_core::analyzer::types::SqlVerb;
use std::cell::RefCell;
use tree_sitter::{Node, Parser};

thread_local! {
    static SQL_PARSER: RefCell<Parser> = RefCell::new({
        let mut p = Parser::new();
        p.set_language(&tree_sitter_sequel::LANGUAGE.into())
            .expect("set tree-sitter-sequel");
        p
    });
}

/// Cheap pre-filter: a leading SQL verb keyword. Avoids tree-sitter work on the
/// overwhelming majority of string literals (log lines, prose, config keys).
pub fn is_sql_shaped(s: &str) -> bool {
    let trimmed = s.trim_start();
    let head: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(head.as_str(), "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH")
}

/// Result of parsing one SQL string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlTables {
    pub tables: Vec<(String, SqlVerb)>,
    pub unresolved: bool,
}

/// Parse a SQL string and return its referenced tables + statement verb.
/// Unparseable / table-less input → `unresolved`.
pub fn parse_tables(sql: &str) -> SqlTables {
    SQL_PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let Some(tree) = parser.parse(sql, None) else {
            return SqlTables { tables: vec![], unresolved: true };
        };
        let root = tree.root_node();
        // A statement that didn't parse cleanly leaves ERROR nodes at/near root.
        let verb = statement_verb(root, sql.as_bytes());
        let Some(verb) = verb else {
            return SqlTables { tables: vec![], unresolved: true };
        };
        let mut tables = Vec::new();
        collect_tables(root, sql.as_bytes(), verb, &mut tables);
        if tables.is_empty() {
            return SqlTables { tables: vec![], unresolved: true };
        }
        SqlTables { tables, unresolved: false }
    })
}

/// Find the first statement node and map it to a verb.
fn statement_verb(node: Node, src: &[u8]) -> Option<SqlVerb> {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "select" => return Some(SqlVerb::Read),
            "insert" | "update" | "delete" => return Some(SqlVerb::Write),
            _ => {}
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Collect table identifiers: `object_reference` nodes whose PARENT is a table
/// position (`from` / `relation` / `insert` / `update` / `delete`), never a
/// `field` (column qualifier like `c.x`).
fn collect_tables(node: Node, src: &[u8], verb: SqlVerb, out: &mut Vec<(String, SqlVerb)>) {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "object_reference" {
            let parent_kind = n.parent().map(|p| p.kind()).unwrap_or("");
            let table_position = matches!(
                parent_kind,
                "from" | "relation" | "insert" | "update" | "delete"
            );
            if table_position {
                if let Some(name_node) = n.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(src) {
                        let owned = name.to_string();
                        if !out.iter().any(|(t, _)| *t == owned) {
                            out.push((owned, verb));
                        }
                    }
                }
            }
        }
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
}
```

- [ ] **Step 4: Wire the module**

In `crates/ecp-analyzer/src/lib.rs`, add alongside the other `pub mod` lines:
```rust
pub mod sql_literal;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ecp-analyzer --test sql_literal_unit`
Expected: PASS (5 tests). If `parse_tables_join_*` fails on column qualifiers,
verify the `parent_kind` allow-list excludes `field`.

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/sql_literal/mod.rs
git add crates/ecp-analyzer/src/sql_literal/mod.rs crates/ecp-analyzer/src/lib.rs crates/ecp-analyzer/tests/sql_literal_unit.rs
git commit -m "feat(sql_literal): language-neutral is_sql_shaped + parse_tables"
```

---

## Task 4: `post_process::sql_table_edges::emit_edges`

**Files:**
- Create: `crates/ecp-analyzer/src/post_process/sql_table_edges.rs`
- Modify: `crates/ecp-analyzer/src/post_process/mod.rs` (add `pub mod sql_table_edges;`)
- Test: covered by Task 7 integration (this task's unit check is the build + a
  focused emit test).

This copies the structure of `post_process::path_literal_nodes::emit_edges` and
`schema_field_mirrors::emit_edges`: resolve the enclosing symbol against
`SymbolTable`, resolve each referenced table name to its `Class` node, emit a
`QueriesTable` edge with `reason = verb`. Unresolved refs and unknown tables are
dropped (no fabricated edge); the unresolved count is returned for telemetry.

- [ ] **Step 1: Read the reference emitter**

Read `crates/ecp-analyzer/src/post_process/path_literal_nodes.rs` fully and
`crates/ecp-analyzer/src/post_process/schema_field_mirrors.rs` lines 39-120 to
match the exact `SymbolTable` lookup API (`symbol_table.resolve(...)`), `uid`,
and `string_pool.add(...)` usage. The emitter signature MUST match the others
so the builder can call it identically.

- [ ] **Step 2: Write the failing test**

`crates/ecp-analyzer/tests/sql_table_edges_unit.rs`:
```rust
// This test drives emit_edges directly with a hand-built SymbolTable + nodes,
// asserting a QueriesTable edge is emitted from the enclosing function to the
// table's Class node with reason "read".
use ecp_analyzer::post_process::sql_table_edges;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_core::analyzer::types::{LocalGraph, RawSqlRef, SqlVerb};
use ecp_core::graph::{Edge, Node, NodeKind, RelType};
use ecp_core::pool::StringPool;
use std::path::PathBuf;

#[test]
fn emits_queries_table_edge_from_function_to_table() {
    // Two nodes: a Function `list_channels` and a Class `channels` (the table).
    let mut pool = StringPool::new();
    let fn_name = pool.add("list_channels");
    let tbl_name = pool.add("channels");
    let empty = pool.add("");
    let mut nodes = vec![
        Node { uid: 1, name: fn_name, file_idx: 0, kind: NodeKind::Function,
               span: (0,0,5,0), community_id: 0, owner_class: empty },
        Node { uid: 2, name: tbl_name, file_idx: 1, kind: NodeKind::Class,
               span: (0,0,3,0), community_id: 0, owner_class: empty },
    ];
    let mut edges: Vec<Edge> = vec![];

    let lg = LocalGraph {
        file_path: PathBuf::from("api/channels.py"),
        sql_refs: Some(vec![RawSqlRef {
            tables: vec![("channels".to_string(), SqlVerb::Read)],
            unresolved: false,
            span: (1,0,1,40),
            enclosing_symbol: Some("list_channels".to_string()),
            enclosing_owner: None,
        }].into_boxed_slice()),
        ..LocalGraph::empty_for_test(PathBuf::from("api/channels.py"))
    };
    let symbol_table = SymbolTable::build_for_test(&nodes, &pool);

    let n = sql_table_edges::emit_edges(&[lg], &symbol_table, &mut pool, &mut nodes, &mut edges);
    assert_eq!(n, 1);
    let e = &edges[0];
    assert_eq!(e.rel_type, RelType::QueriesTable);
    let reason = pool.get(e.reason);
    assert_eq!(reason, "read");
}
```

> **Note:** `LocalGraph::empty_for_test` and `SymbolTable::build_for_test` may
> not exist. STEP 3a covers adding minimal test helpers if the existing test
> suite lacks them — check `crates/ecp-analyzer/tests/` for how other
> post_process tests construct a `SymbolTable` first, and reuse that exact
> approach instead of inventing helpers.

- [ ] **Step 3a: Check existing test construction pattern**

Run: `grep -rn "SymbolTable" crates/ecp-analyzer/tests/*.rs | head`
Use whatever construction the existing post_process tests use. If none drive
`emit_edges` directly, make THIS test build the `SymbolTable` the same way
`builder.rs` does (read `builder.rs` around line 1600 for the construction). Do
NOT invent `*_for_test` helpers if the real construction is straightforward.

- [ ] **Step 3b: Run test to verify it fails**

Run: `cargo test -p ecp-analyzer --test sql_table_edges_unit`
Expected: FAIL — `sql_table_edges` module not found.

- [ ] **Step 4: Implement `crates/ecp-analyzer/src/post_process/sql_table_edges.rs`**

```rust
//! `QueriesTable` edge emission from `LocalGraph.sql_refs`.
//!
//! Each `RawSqlRef` resolves to: the enclosing Function/Method (via
//! `SymbolTable`) → the referenced table's `Class` node (via `SymbolTable`).
//! Emits one `QueriesTable` edge per (symbol, table) with `reason = verb`.
//!
//! Honest-no-data: an unresolved ref, an unresolvable enclosing symbol, or a
//! table name with no matching `Class` node yields NO edge — never a fabricated
//! source or sink. Returns the emitted-edge count for builder telemetry.

use crate::resolution::index::SymbolTable;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{Edge, RelType};
use ecp_core::pool::StringPool;

pub fn emit_edges(
    local_graphs: &[LocalGraph],
    symbol_table: &SymbolTable,
    string_pool: &mut StringPool,
    nodes: &mut Vec<ecp_core::graph::Node>,
    edges: &mut Vec<Edge>,
) -> usize {
    let _ = nodes; // table + function nodes already exist; we only add edges
    let mut edge_count = 0usize;

    for lg in local_graphs.iter() {
        let Some(ref refs) = lg.sql_refs else { continue };
        if refs.is_empty() {
            continue;
        }
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");

        for raw in refs.iter() {
            if raw.unresolved {
                continue; // BlindSpot handled in Task 9; no edge here
            }
            let Some(sym_name) = raw.enclosing_symbol.as_deref() else {
                continue; // module-top-level SQL: no caller to attribute
            };
            // Resolve the enclosing function/method node index. Use the SAME
            // resolution call the reference emitters use (see path_literal_nodes
            // / schema_field_mirrors for the exact SymbolTable method + owner
            // disambiguation). Pseudocode shape:
            let Some(src_idx) = symbol_table.resolve_symbol_in_file(
                sym_name,
                raw.enclosing_owner.as_deref(),
                &path_str,
            ) else {
                continue;
            };

            for (table, verb) in raw.tables.iter() {
                let Some(tgt_idx) = symbol_table.resolve_table_class(table) else {
                    continue; // unknown table → drop (false hit)
                };
                let reason_ref = string_pool.add(verb.as_reason());
                edges.push(Edge {
                    source: src_idx,
                    target: tgt_idx,
                    rel_type: RelType::QueriesTable,
                    confidence: 1.0,
                    reason: reason_ref,
                });
                edge_count += 1;
            }
        }
    }
    edge_count
}
```

> **IMPLEMENTER NOTE:** `resolve_symbol_in_file` and `resolve_table_class` are
> placeholders for whatever `SymbolTable` actually exposes. Read
> `path_literal_nodes.rs` (resolves enclosing symbol) and `schema_field_mirrors.rs`
> (resolves a class by name) and use THOSE real methods. The `Edge` struct fields
> are `source: u32, target: u32, rel_type: RelType, confidence: f32, reason: StrRef`
> (verified against `graph.rs`) — all five are required when constructing one.

- [ ] **Step 5: Wire the module**

In `crates/ecp-analyzer/src/post_process/mod.rs`, add (alphabetical-ish, near `schema_field_mirrors`):
```rust
pub mod sql_table_edges;
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p ecp-analyzer --test sql_table_edges_unit`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/post_process/sql_table_edges.rs
git add crates/ecp-analyzer/src/post_process/sql_table_edges.rs crates/ecp-analyzer/src/post_process/mod.rs crates/ecp-analyzer/tests/sql_table_edges_unit.rs
git commit -m "feat(post_process): emit QueriesTable edges from sql_refs"
```

---

## Task 5: Wire `sql_table_edges` into the builder

**Files:**
- Modify: `crates/ecp-analyzer/src/resolution/builder.rs` (near line 1650, where the other `post_process::*::emit_edges` calls live)

- [ ] **Step 1: Read the call sequence**

Read `crates/ecp-analyzer/src/resolution/builder.rs` lines 1600-1700. The
post_process emitters are called in a sequence, each `let n = post_process::X::emit_edges(&local_graphs, &symbol_table, &mut string_pool, &mut nodes, &mut edges);`
followed by a debug log. Match that exact shape.

- [ ] **Step 2: Add the call**

After the `schema_field_mirrors::emit_edges(...)` call (line ~1650), add:
```rust
    let sql_table_edge_count = crate::post_process::sql_table_edges::emit_edges(
        &local_graphs,
        &symbol_table,
        &mut string_pool,
        &mut nodes,
        &mut edges,
    );
    tracing::debug!(sql_table_edges = sql_table_edge_count, "emitted QueriesTable edges");
```

> Match the surrounding code's exact variable names (`local_graphs`,
> `symbol_table`, `string_pool`, `nodes`, `edges`) — they may differ; read the
> adjacent calls and copy their argument expressions verbatim.

- [ ] **Step 3: Build to verify**

Run: `cargo build -p ecp-analyzer`
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/resolution/builder.rs
git add crates/ecp-analyzer/src/resolution/builder.rs
git commit -m "feat(builder): run sql_table_edges post-process pass"
```

---

## Task 6: Python parser — feed `sql_refs`

**Files:**
- Modify: `crates/ecp-analyzer/src/python/receiver_types.rs` (the `extract_python_calls_and_path_literals` fn at line 158 — extend it to ALSO collect `RawSqlRef`s, or add a sibling pass)
- Modify: `crates/ecp-analyzer/src/python/parser.rs` (around line 1285/1465 where `raw_path_literals` is collected and assigned to `LocalGraph.path_literals` — assign `sql_refs` the same way)

The Python parser already walks `string_literal` captures for path literals.
For each such literal, ALSO test `sql_literal::is_sql_shaped`; if true, run
`parse_tables` and push a `RawSqlRef` carrying the enclosing symbol/owner (the
same enclosing-symbol resolution the path-literal pass already computes).

- [ ] **Step 1: Read the existing extraction**

Read `crates/ecp-analyzer/src/python/receiver_types.rs` lines 158-260 to see how
each string literal's value is obtained (quote-stripping) and how
`enclosing_symbol` / `enclosing_owner` are resolved for `RawPathLiteral`. The
SQL pass reuses EXACTLY those resolved values.

- [ ] **Step 2: Write the failing integration test**

`crates/ecp-analyzer/tests/python_sql_table_edges.rs`:
```rust
// Parse a small Python source that defines a function querying `channels`,
// alongside a CREATE TABLE so the table Class node exists, then build the graph
// and assert a QueriesTable edge from the function to the table.
use ecp_analyzer::test_support::build_graph_from_sources; // see Step 3 note

#[test]
fn python_select_emits_read_edge_to_table() {
    let sources = &[
        ("schema.sql", "CREATE TABLE channels (id BIGINT PRIMARY KEY, slug TEXT);"),
        ("api.py", "def list_channels(pool):\n    return pool.fetch(\"SELECT id, slug FROM channels WHERE org_id = $1\")\n"),
    ];
    let graph = build_graph_from_sources(sources);
    assert!(
        graph.has_edge_kind("list_channels", "channels", "QueriesTable", "read"),
        "expected QueriesTable(read) list_channels → channels"
    );
}

#[test]
fn python_update_emits_write_edge() {
    let sources = &[
        ("schema.sql", "CREATE TABLE channels (id BIGINT PRIMARY KEY, slug TEXT);"),
        ("svc.py", "def rename(pool):\n    pool.execute(\"UPDATE channels SET slug = $1 WHERE id = $2\")\n"),
    ];
    let graph = build_graph_from_sources(sources);
    assert!(graph.has_edge_kind("rename", "channels", "QueriesTable", "write"));
}
```

> **Step 3 note — test harness:** `build_graph_from_sources` / `has_edge_kind`
> are conveniences that almost certainly need to be located, not invented.
> Run `grep -rn "fn build.*graph\|fn has_edge\|ZeroCopyGraph" crates/ecp-analyzer/tests/*.rs crates/ecp-analyzer/src/ | head` and use the
> existing in-repo graph-build-from-source test utility (the SQL DDL tests and
> impact tests already build graphs from fixtures). Adapt the assertions to that
> utility's actual API. If truly absent, build the graph via the same path
> `builder.rs` uses and query `edges` for a `QueriesTable` with the right
> source/target/reason.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ecp-analyzer --test python_sql_table_edges`
Expected: FAIL — no QueriesTable edge yet (Python parser doesn't feed sql_refs).

- [ ] **Step 4: Implement the Python hook**

In `extract_python_calls_and_path_literals` (receiver_types.rs:158), add a
`Vec<RawSqlRef>` accumulator next to the `RawPathLiteral` one. For each string
literal already being inspected, after the path-literal check, add:

```rust
    // SQL-string detection rides the same literal walk as path literals.
    if crate::sql_literal::is_sql_shaped(literal_value) {
        let parsed = crate::sql_literal::parse_tables(literal_value);
        raw_sql_refs.push(ecp_core::analyzer::types::RawSqlRef {
            tables: parsed.tables,
            unresolved: parsed.unresolved,
            span: literal_span,
            enclosing_symbol: enclosing_symbol.clone(),
            enclosing_owner: enclosing_owner.clone(),
        });
    }
```

Change the fn's return type to also return the `Vec<RawSqlRef>` (a tuple), and
in `parser.rs` (line ~1465) assign:
```rust
    let sql_refs = (!raw_sql_refs.is_empty()).then(|| raw_sql_refs.into_boxed_slice());
```
then add `sql_refs,` to the `LocalGraph { … }` construction (line ~1481).

> Match the actual variable names (`literal_value`, `literal_span`,
> `enclosing_symbol`, `enclosing_owner`) to what the existing path-literal code
> binds — read lines 158-260 first and reuse those exact bindings.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ecp-analyzer --test python_sql_table_edges`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/python/receiver_types.rs crates/ecp-analyzer/src/python/parser.rs
git add crates/ecp-analyzer/src/python/ crates/ecp-analyzer/tests/python_sql_table_edges.rs
git commit -m "feat(python): feed sql_refs → QueriesTable edges"
```

---

## Task 7: Go + Rust parsers — feed `sql_refs`

**Files:**
- Modify: `crates/ecp-analyzer/src/go/parser.rs` (Go: backtick raw strings)
- Modify: `crates/ecp-analyzer/src/rust/parser.rs` (Rust: `sqlx::query!("…")`, plain string args)
- Test: `crates/ecp-analyzer/tests/go_sql_table_edges.rs`, `crates/ecp-analyzer/tests/rust_sql_table_edges.rs`

Same mechanism as Task 6, applied to Go and Rust. These two exercise the
distinct string shapes (Go backtick, Rust macro/raw-string) that Task 8's
fan-out replicates.

- [ ] **Step 1: Locate each language's path-literal / string-literal hook**

Run: `grep -rn "RawPathLiteral\|is_path_shaped\|path_literals" crates/ecp-analyzer/src/go/ crates/ecp-analyzer/src/rust/`
Identify the same hook point Task 6 used for Python in each parser.

- [ ] **Step 2: Write the failing tests**

`crates/ecp-analyzer/tests/go_sql_table_edges.rs`:
```rust
use ecp_analyzer::test_support::build_graph_from_sources;

#[test]
fn go_backtick_select_emits_read_edge() {
    let sources = &[
        ("schema.sql", "CREATE TABLE channels (id BIGINT PRIMARY KEY, slug TEXT);"),
        ("store.go", "package store\nfunc ListChannels(db *sql.DB) {\n  db.Query(`SELECT id, slug FROM channels WHERE org_id = $1`)\n}\n"),
    ];
    let graph = build_graph_from_sources(sources);
    assert!(graph.has_edge_kind("ListChannels", "channels", "QueriesTable", "read"));
}
```

`crates/ecp-analyzer/tests/rust_sql_table_edges.rs`:
```rust
use ecp_analyzer::test_support::build_graph_from_sources;

#[test]
fn rust_sqlx_select_emits_read_edge() {
    let sources = &[
        ("schema.sql", "CREATE TABLE channels (id BIGINT PRIMARY KEY, slug TEXT);"),
        ("store.rs", "fn list_channels(pool: &Pool) {\n    sqlx::query(\"SELECT id, slug FROM channels WHERE org_id = $1\");\n}\n"),
    ];
    let graph = build_graph_from_sources(sources);
    assert!(graph.has_edge_kind("list_channels", "channels", "QueriesTable", "read"));
}
```

(Use the same real harness located in Task 6 Step 3.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ecp-analyzer --test go_sql_table_edges --test rust_sql_table_edges`
Expected: FAIL — Go/Rust parsers don't feed sql_refs yet.

- [ ] **Step 4: Implement both hooks**

Apply the Task-6 pattern to `go/parser.rs` and `rust/parser.rs`: at each
parser's string-literal inspection point, call `is_sql_shaped` → `parse_tables`
→ push `RawSqlRef` with that parser's resolved enclosing symbol/owner, then
assign `sql_refs` in the `LocalGraph` construction. Go's backtick raw strings
are already captured as `string_literal` by its tree-sitter query; the value is
the same `&str` fed to `is_sql_shaped`.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ecp-analyzer --test go_sql_table_edges --test rust_sql_table_edges`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/go/parser.rs crates/ecp-analyzer/src/rust/parser.rs
git add crates/ecp-analyzer/src/go/ crates/ecp-analyzer/src/rust/ crates/ecp-analyzer/tests/go_sql_table_edges.rs crates/ecp-analyzer/tests/rust_sql_table_edges.rs
git commit -m "feat(go,rust): feed sql_refs → QueriesTable edges"
```

---

## Task 8: Fan-out to the remaining 11 mainstream languages

**Files (one parser + one test each):**
- Modify: `typescript`, `javascript`, `java`, `kotlin`, `c_sharp`, `php`, `ruby`, `swift`, `c`, `cpp`, `dart` parsers under `crates/ecp-analyzer/src/<lang>/parser.rs`
- Test: `crates/ecp-analyzer/tests/<lang>_sql_table_edges.rs` for each

Mechanical replication of Task 6/7 across the 11 remaining mainstream languages
required by the 14-language rule. Each language gets the identical hook + one
integration test.

- [ ] **Step 1: For each of the 11 languages, locate its string-literal hook**

Run per language: `grep -rn "RawPathLiteral\|path_literals" crates/ecp-analyzer/src/<lang>/`

- [ ] **Step 2: Write one integration test per language**

For each `<lang>`, create `crates/ecp-analyzer/tests/<lang>_sql_table_edges.rs`
with a single test: a `schema.sql` with `CREATE TABLE channels (...)` plus a
minimal `<lang>` source whose function issues `SELECT id FROM channels`, then
assert `QueriesTable(read)` from the function to `channels`. Use the language's
idiomatic raw-SQL call (e.g. Java `jdbcTemplate.query("…")`, C# `conn.Query("…")`,
PHP `$pdo->query("…")`, Ruby `conn.exec("…")`). Reuse `build_graph_from_sources`.

- [ ] **Step 3: Run to confirm all 11 fail**

Run: `cargo test -p ecp-analyzer sql_table_edges 2>&1 | grep -E "FAILED|test result"`
Expected: the 11 new language tests FAIL (no sql_refs fed yet).

- [ ] **Step 4: Apply the Task-6 hook to each parser**

For each `<lang>/parser.rs`, add the `is_sql_shaped` → `parse_tables` → push
`RawSqlRef` → assign `sql_refs` pattern at its string-literal hook. Run that
language's test after each to confirm green before moving to the next.

- [ ] **Step 5: Run the full matrix**

Run: `cargo test -p ecp-analyzer sql_table_edges`
Expected: all 14 language tests PASS (Python, Go, Rust from Tasks 6/7 + these 11).

- [ ] **Step 6: Commit**

```bash
rustfmt --edition 2021 <each touched parser.rs>
git add crates/ecp-analyzer/src crates/ecp-analyzer/tests
git commit -m "feat(parsers): feed sql_refs for remaining 11 mainstream languages"
```

> **If any language's string capture differs** (e.g. C++ raw string literals
> `R"(...)"`, Swift multiline `\"\"\"`): the value handed to `is_sql_shaped` must
> be the unquoted SQL text. If a language's existing path-literal pass already
> strips quotes correctly, the SQL pass inherits that for free. Note any language
> that needs special unquoting as a follow-up rather than blocking the matrix.

---

## Task 9: BlindSpot for dynamic table names + end-to-end verification

**Files:**
- Modify: `crates/ecp-analyzer/src/post_process/sql_table_edges.rs` (emit a `BlindSpot` for unresolved refs)
- Test: `crates/ecp-analyzer/tests/sql_table_edges_blindspot.rs`

- [ ] **Step 1: Read how BlindSpots are emitted**

Run: `grep -rn "BlindSpot\|blind_spot" crates/ecp-analyzer/src/post_process/ crates/ecp-core/src/ | head`
Match the existing `BlindSpot` construction shape used elsewhere.

- [ ] **Step 2: Write the failing test**

`crates/ecp-analyzer/tests/sql_table_edges_blindspot.rs`:
```rust
use ecp_analyzer::test_support::build_graph_from_sources;

#[test]
fn dynamic_table_name_yields_blindspot_not_edge() {
    // f-string interpolates the TABLE name → unresolvable → BlindSpot, no edge.
    let sources = &[
        ("schema.sql", "CREATE TABLE channels (id BIGINT PRIMARY KEY);"),
        ("svc.py", "def run(pool, tbl):\n    pool.execute(f\"SELECT * FROM {tbl} WHERE id = $1\")\n"),
    ];
    let graph = build_graph_from_sources(sources);
    assert!(!graph.has_any_edge_kind("run", "QueriesTable"),
            "dynamic table must NOT produce a fabricated edge");
    assert!(graph.has_blindspot_containing("dynamic"),
            "dynamic table should surface as a BlindSpot");
}
```

> Adapt `has_any_edge_kind` / `has_blindspot_containing` to the real test
> harness API located in Task 6. The key assertions: no QueriesTable edge, and a
> BlindSpot recorded.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p ecp-analyzer --test sql_table_edges_blindspot`
Expected: FAIL — no BlindSpot emitted yet (or a fabricated edge appears).

> **Dependency:** the Python parser must mark interpolated-table SQL as
> `unresolved`. The POC's approach (substitute `{expr}` with a placeholder
> identifier, then a STATIC table still parses while a dynamic table parses to
> the placeholder name) lives in `sql_literal`. Ensure `parse_tables` returns
> `unresolved: true` (or a placeholder table that the table-resolution step
> drops) when the table position is interpolated. If Python's literal walk
> hands the raw f-string text (with `{tbl}`) to `is_sql_shaped`, `parse_tables`
> will see `{tbl}` in the table position and tree-sitter-sequel will fail to
> resolve a real identifier there → `unresolved`. Verify this and add a
> `sql_literal` unit test pinning it:
> ```rust
> #[test]
> fn parse_tables_interpolated_table_is_unresolved() {
>     let r = parse_tables("SELECT * FROM {tbl} WHERE id = $1");
>     assert!(r.unresolved);
> }
> ```

- [ ] **Step 4: Implement BlindSpot emission**

In `sql_table_edges::emit_edges`, where `raw.unresolved` is currently
`continue`d, instead record a `BlindSpot` (matching the construction from Step 1)
describing the unresolved dynamic SQL at the ref's span/symbol, then continue.
Add the `BlindSpot` accumulator to the fn signature if needed (match how other
passes surface BlindSpots — some attach to `LocalGraph`, some return them).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p ecp-analyzer --test sql_table_edges_blindspot`
Expected: PASS.

- [ ] **Step 6: End-to-end smoke against the real index**

Build the release binary and reindex a fixture (or enoract) to confirm the edge
appears via the actual `ecp impact` path:
```bash
cargo build -p egent-code-plexus --bin ecp --release
./target/release/ecp admin index --force --repo <fixture-with-sql>
./target/release/ecp impact channels --direction upstream
```
Expected: the code function(s) that SELECT/UPDATE `channels` appear in the
upstream impact set with `QueriesTable` provenance. Record actual output.

- [ ] **Step 7: Run the FULL analyzer suite (no regression)**

Run: `cargo test -p ecp-analyzer`
Expected: all pass — existing SQL DDL, path_literal, and schema_field tests
unaffected by the new capture path.

- [ ] **Step 8: Commit**

```bash
rustfmt --edition 2021 crates/ecp-analyzer/src/post_process/sql_table_edges.rs
git add crates/ecp-analyzer/src/post_process/sql_table_edges.rs crates/ecp-analyzer/tests/sql_table_edges_blindspot.rs crates/ecp-analyzer/tests/sql_literal_unit.rs
git commit -m "feat(sql_table_edges): BlindSpot for dynamic table names + e2e verify"
```

---

## Self-Review Notes (filled during authoring)

- **Spec coverage:** RelType (T1) · RawSqlRef/sql_refs (T2) · is_sql_shaped+parse_tables neutral core (T3) · edge emitter w/ honest-no-data (T4) · builder wiring (T5) · per-lang hook Python/Go/Rust (T6/T7) · 11-lang fan-out (T8) · BlindSpot + e2e (T9). All spec sections mapped.
- **A/B/C gate:** documented in T1's doc-comment (gate A — graph completeness).
- **Schema stability:** T1 appends RelType at END (rkyv rule). No NodeKind added.
- **14-lang rule:** T6/T7 (3 representative) + T8 (11 fan-out) = 14.
- **Deferred (spec open questions):** per-lang interpolation normalization beyond Python (T8 note flags special unquoting as follow-up); column-level edges (table-level only this round); is_sql_shaped precision tuning (verb-keyword prefilter chosen).
- **Implementer-resolved unknowns (flagged inline, not placeholders):** exact `SymbolTable` resolution methods (T4 reads path_literal_nodes/schema_field_mirrors), exact `Edge` field names (T4 verifies vs graph.rs), test harness `build_graph_from_sources` (T6 locates real utility). These are "read the reference and match it" instructions, not unfilled TBDs.
