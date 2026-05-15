# graph-nexus Parser Expansion — Fill gaps + add new languages

**Goal:** Close the 20 missing-feature cells in the current 14-language parity matrix and add 6+ new languages (starting with Lua), using parallel Sonnet subagents.

**Approach:** Decompose the work into three layers — (1) a small Phase 0 setup (worker brief + Swift capture-name fix + verification harness), (2) a Sonnet worker template for per-language pattern-fill tasks fanned out in parallel, (3) a shared config-file detection module that several languages plug into.

**Date:** 2026-05-14

---

## 1. Background

All language support in `graph-nexus` is implemented via `tree-sitter` + a `queries.scm` capture file + a thin Rust parser at `crates/graph-nexus-analyzer/src/<lang>/parser.rs`. Parser files range from 127 lines (C, the simplest) to 254 lines (TypeScript, the most complete, which includes route-handling and a dedup loop the other parsers lack — when templating a new language, prefer C or Rust as the base, not TS, to avoid copying TS-specific logic). The depth of language support is roughly proportional to the size of `queries.scm`:

| Lang | queries.scm | Missing cells |
|---|---:|---:|
| TypeScript | 136 | 0 |
| Java | 77 | 1 |
| Go | 77 | 1 |
| C# | 72 | 0 |
| Rust | 67 | 1 |
| C++ | 64 | 3 |
| JavaScript | 53 | 1 |
| Ruby | 52 | 3 |
| Python | 50 | 0 |
| PHP | 47 | 1 |
| Kotlin | 40 | 1 |
| Dart | 38 | 2 |
| C | 24 | 4 |
| Swift | 22 | 2 |

Total: 20 missing cells across 11 languages. The Rust scaffolding around each parser is stable and uniform; expanding coverage = writing more tree-sitter queries plus a small shared `config_detector` module for the Config axis. Constructor Inference, Frameworks, and Entry Points are already ✓ across all 14 languages and need no per-language work — they're either inherited from the shared `extract_calls` infrastructure or already captured by every parser's existing queries.

## 2. Gap matrix (the 20 cells)

Cells marked `—` in the parity table, mapped to concrete work items:

| Lang | Cell | Work |
|---|---|---|
| JS | Type Annot. | TS-style `@type` captures don't apply; skip with rationale, or pull JSDoc `@param`/`@returns` comments |
| Java | Config | Register `pom.xml` / `build.gradle` as config files (shared infrastructure — once for all langs) |
| Kotlin | Config | Register `build.gradle.kts` (shared) |
| Go | Named Bindings | `import ( "a" "b/c" )` group form — extract per-package aliases (`import alias "path"`) |
| Rust | Config | Register `Cargo.toml` (shared) |
| PHP | Heritage | `class Foo extends Bar implements Iface1, Iface2` — add `superclass` + `interfaces` captures |
| Ruby | Named Bindings | `require_relative '...'` doesn't have named bindings; rationale-only |
| Ruby | Type Annot. | No native types; skip unless RBS sidecar — rationale-only |
| Ruby | Config | Register `Gemfile` / `.gemspec` (shared) |
| Swift | Imports | `import struct Foo.Bar` form — extend `import_declaration` capture |
| Swift | Named Bindings | `import struct/class/protocol` specific-symbol form |
| C | Imports | `#include <...>` vs `"..."` distinguish system vs local; already captured but not classified |
| C | Named Bindings | C has no named imports; rationale-only |
| C | Heritage | C has no inheritance; rationale-only |
| C | Config | Register `Makefile` / `CMakeLists.txt` (shared) |
| C++ | Imports | `#include` + `using namespace` + `using X = Y` — three separate forms |
| C++ | Named Bindings | `using std::vector` form |
| C++ | Config | Register `Makefile` / `CMakeLists.txt` (shared with C) |
| Dart | Named Bindings | `import 'x.dart' show A, B` / `hide` — `show_combinator` / `hide_combinator` captures |
| Dart | Config | Register `pubspec.yaml` (shared) |

Two classes:
-   **Query-only** (13 items): pure `queries.scm` edits, no Rust changes. Trivially parallelizable. (Note: 3 of these are rationale-only — Ruby Named Bindings / Ruby Type Annot / C Named Bindings / C Heritage — where the language genuinely lacks the feature and the "fix" is documenting why.)
-   **Shared-code Config** (7 items): all funnel into a single `config_detector` module with per-language file-pattern entries. Build the module once, then add 7 rows.

## 3. Parallelization strategy

### 3.1 Cost rationale

| | Sonnet 4.6 | Opus 4.7 |
|---|---:|---:|
| Input $/M | $3 | $15 |
| Output $/M | $15 | $75 |
| Est. cost per language task | ~$0.17 | ~$0.83 |
| 26 tasks total (20 gaps + Lua + 5 new) | **~$4** | **~$22** |

Tree-sitter query writing is high-template, low-novel-reasoning work: Sonnet is the right default. Reserve Opus only for Phase 0 (small) and final parity validation if results look suspicious.

### 3.2 Subagent isolation considerations

Each subagent is a fresh context — it must re-read sample parsers, `queries.scm` templates, and the target grammar's `node-types.json`. To avoid 26× duplicated reads:

-   **Shared brief**: write a single `docs/plans/parser-worker-brief.md` containing (a) the parser template anatomy, (b) capture naming conventions, (c) verification commands. Each subagent's prompt opens with "read this brief, then [task-specific instructions]" — the brief's content becomes a prompt-cache hit if subagents fire within the 5-minute window.
-   **No shared mutable state during fan-out**: each subagent works on one `crates/graph-nexus-analyzer/src/<lang>/queries.scm` and nothing else. The Config wirings are batched in a separate serial phase (§3.3 Phase 2) so they all touch `config_detector` cleanly.

### 3.3 Batch table

| Phase | Tasks | Model | Concurrency | Duration |
|---|---|---|---|---|
| **0. Setup** | (a) worker brief, (b) Swift capture-name fix (see §4), (c) confirm verification harness runs, (d) trial run on one cell end-to-end | Opus or Sonnet 1× | serial | ~2 hr |
| **1. Query-only gaps** | 13 cells across 9 langs (mix of substantive + rationale-only) | Sonnet | 8 in parallel × 2 waves | ~1 day |
| **2. Config detector** | Build `config_detector` module + register 7 file-pattern entries (Cargo.toml, pom.xml + build.gradle, build.gradle.kts, Gemfile + .gemspec, Makefile + CMakeLists.txt, pubspec.yaml) | Sonnet 1× | serial | ~2 hr |
| **3. New language: Lua** | New crate dir + queries.scm + fixture | Sonnet 1× | independent of phase 1 | ~half day |
| **4. New languages: Bash / SQL / HCL / Scala / Elixir** | Same template, 5 langs | Sonnet | 5 in parallel | ~1 day |
| **5. Parity validation** | Run parity harness, file-by-file diff against upstream gitnexus output | Sonnet 1× | serial | ~half day |

Total wall-clock: **~2.5 days** with parallelization vs ~6 days serial. Total est. cost: **$3–$5**.

## 4. Worker brief contents

The shared `docs/plans/parser-worker-brief.md` (to be written in Phase 0) must include:

0.  **Phase 0 hard prerequisite — Swift capture-name alignment**: `crates/graph-nexus-analyzer/src/swift/queries.scm` currently uses `@name.class` / `@name.function`, but `swift/parser.rs` calls `capture_index_for_name("class.name")` etc. (the standard convention used by the other 13 langs). The mismatch means Swift currently produces empty parse output silently — no error, no symbols. **Fix this before any Swift gap-fill worker runs**, otherwise the workers will spin on queries that look correct but resolve to empty captures. Standardize on `@class.name` / `@function.name` to match the other parsers.
1.  **Anatomy of a parser**: walk through `c/parser.rs` (smallest reference, 127 lines) — `Provider::new` loads grammar + query, `parse_file` runs query, captures iterated, `RawNode` / `RawImport` emitted into `LocalGraph`.
2.  **Anatomy of `queries.scm`**: capture naming convention (`@function.name`, `@function`, `@import.source`, `@import`, `@const.name`, `@struct.name`, `@struct`, `@export`, `@heritage`, `@type`, `@decorator`). Reference: TS as the gold standard.
3.  **Adding a new capture name**: corresponding handler in `parser.rs` `capture_index_for_name(...)` block + `RawNode` field mapping.
4.  **Verification recipe**:
    ```bash
    cargo build -p graph-nexus-analyzer
    cargo test -p graph-nexus-analyzer --test ast_test   # or per-lang fixture
    gnx admin index --repo tests/parity/fixtures/<lang>/sample_project
    gnx inspect tests/parity/fixtures/<lang>/sample_project:<known_symbol>
    ```
5.  **Failure mode catalogue**: wrong node name (`class_declaration` vs `class_definition`), capture index `None` returned (capture present in `.scm` but not in match — usually optional `?` issue), tree-sitter version mismatch (some grammars on `0.20` API), capture-name vs `capture_index_for_name` mismatch (the Swift bug above generalised).
6.  **Hard constraint**: the worker MUST run `cargo build` and at least one query against a fixture, and report stdout, before declaring done. No "looks correct" claims.

## 5. Lua addition — concrete spec

### 5.1 Dependencies

```toml
# crates/graph-nexus-analyzer/Cargo.toml
tree-sitter-lua = "0.4"  # pin once selected
```

### 5.2 Files to create

```
crates/graph-nexus-analyzer/src/lua/
├── mod.rs           # pub mod parser;
├── parser.rs        # ~180 lines, copy c/parser.rs and adapt
└── queries.scm      # ~70 lines, draft below
```

### 5.3 `queries.scm` draft

```scheme
;; Functions — top-level
(function_declaration
  name: (identifier) @function.name) @function

;; M.foo = function(...) end  (table-method form)
(function_declaration
  name: (dot_index_expression
    field: (identifier) @function.name)) @function

;; obj:method(...) (colon method form, declares self-bound method)
(function_declaration
  name: (method_index_expression
    method: (identifier) @function.name)) @function

;; local function foo() end
(local_function
  name: (identifier) @function.name) @function

;; Anonymous function assigned to variable: local foo = function() end
(variable_declaration
  (assignment_statement
    (variable_list (variable name: (identifier) @function.name))
    (expression_list (function_definition)))) @function

;; Variables / Constants — top-level
(variable_declaration
  (assignment_statement
    (variable_list (variable name: (identifier) @const.name)))) @const

;; Table-as-class heuristic — local T = {} on a line whose name matches PascalCase
;; (Note: requires post-filter in parser.rs to apply naming heuristic)
(variable_declaration
  (assignment_statement
    (variable_list (variable name: (identifier) @struct.name))
    (expression_list (table_constructor)))) @struct

;; Imports — Lua has no native imports, but require("mod") is canonical
(function_call
  name: (identifier) @_fn
  arguments: (arguments (string content: _ @import.source))
  (#eq? @_fn "require")) @import
```

### 5.4 Parser registration

Two sites in `crates/graph-nexus-cli/src/commands/analyze.rs` must be updated together:

```rust
// 1. Extension routing — extend the `match ext` arm that picks a provider:
match ext {
    "lua" | "luau" => /* route to lua provider */,
    // ... existing arms ...
}

// 2. Provider initialization — add a register_provider call alongside the others:
pipeline.register_provider(Box::new(
    graph_nexus_analyzer::lua::parser::LuaProvider::new()?
));
```

Both edits live in the same file; do them together to avoid the common pitfall of registering the provider but forgetting the extension routing (the provider then silently never runs on `.lua` files).

Also: `crates/graph-nexus-analyzer/src/lib.rs` needs `pub mod lua;`.

### 5.5 Fixture

```
tests/parity/fixtures/lua/sample_project/
├── init.lua              # uses require, defines functions
├── module_a.lua          # exports a table with methods
└── README.md
```

Path convention matches the existing `tests/parity/fixtures/basic/` (TypeScript). Each new language gets a sibling directory under `tests/parity/fixtures/`.

### 5.6 Expected coverage

| Cell | Status | Notes |
|---|---|---|
| Imports | ✓ | `require("mod")` |
| Named Bindings | — | Lua's `require` returns the table; named binding is `local X = require("...")` — could capture, future work |
| Exports | — | Module-level `return { ... }` is the convention; non-trivial heuristic |
| Heritage | — | `setmetatable(Child, { __index = Parent })` — pattern-detectable, future work |
| Type Annotations | — | Vanilla Lua untyped; Luau has annotations (future) |
| Constructor Inference | ✓ | Inherited from shared `extract_calls` — no per-language work needed |
| Config | ✓ | `.luarc.json`, `init.lua` as entry (wire into `config_detector`) |
| Frameworks | ~ | LÖVE (`love.load`/`love.update`), Neovim plugin (`require('plenary')`) — future |
| Entry Points | ✓ | Top-level `function main` or returned module |

Expected initial parity: **6/9 columns**, comparable to Ruby's coverage.

## 6. Additional languages (Phase 4)

| Language | tree-sitter crate | Initial scope | Effort |
|---|---|---|---|
| **Bash** | `tree-sitter-bash` | functions, `source`/`.` imports, env vars as const | ½ day |
| **SQL** | `tree-sitter-sql` | `CREATE FUNCTION` / `CREATE VIEW` / `CREATE PROCEDURE` as defines; table refs as imports | ½ day |
| **HCL / Terraform** | `tree-sitter-hcl` | `resource` / `module` / `data` blocks as classes; module imports | ½ day |
| **Scala** | `tree-sitter-scala` | full OO + functional — most complete coverage achievable | 1 day |
| **Elixir** | `tree-sitter-elixir` | `defmodule` / `def` / `defp`, `alias` / `import` / `use` | 1 day |

Each goes through the same template as Lua. Bash + SQL + HCL group is particularly valuable because **they're often cross-file targets from existing languages** (Python calling shell, application code embedding SQL strings, Terraform consumed by CI/CD scripts) — adding them creates new edges in repos that already have multiple languages indexed.

## 7. Build sequence

```
Day 1 (Opus or Sonnet, serial)
  └─ Phase 0: setup
     ├─ docs/plans/parser-worker-brief.md
     ├─ Swift capture-name alignment fix (queries.scm ↔ parser.rs)
     ├─ Verification harness sanity check (scripts/parity/run_parity.py)
     └─ One trial cell run end-to-end (e.g. JS Type Annotation)

Day 2 AM (Sonnet, parallel × 8)
  └─ Phase 1 wave 1: 8 query-only cells
Day 2 PM (Sonnet, parallel × 5)
  └─ Phase 1 wave 2: 5 query-only cells
Day 2 PM (Sonnet, serial)
  └─ Phase 2: config_detector module + 7 file-pattern entries

Day 3 (Sonnet, parallel × 6)
  └─ Phase 3+4: Lua + Bash + SQL + HCL + Scala + Elixir

Day 3 PM (Sonnet, serial)
  └─ Phase 5: parity validation + cleanup
```

## 8. Dispatch checklist

Before Phase 1 fan-out, verify:

-   [ ] Worker brief written and validated against one trial task
-   [ ] Swift capture-name alignment merged (`queries.scm` ↔ `parser.rs` consistent)
-   [ ] `config_detector` module structure scaffolded (so Phase 2 has a clear target)
-   [ ] One trial subagent run completed successfully (e.g. JS Type Annotation → confirm flow works end-to-end including `cargo build` + fixture query)
-   [ ] Verification harness command works (no false negatives)
-   [ ] At-most-N concurrent agents decided (recommend 6–8 to stay under rate limits)

## 9. Non-goals

-   **Framework-specific extraction** (React component graph, Spring beans, Rails routes) — separate spec; can layer on once base parsers are solid.
-   **Type inference across files** — keep type captures local; full type resolution is `resolution/` crate's concern, not the per-language parser's.
-   **Refactoring tools per language** — `rename`-style operations stay generic for now.
-   **LSP integration** — out of scope; graph-nexus is an indexer, not a language server.
-   **Per-language Constructor Inference refactor** — Constructor calls are currently handled uniformly by `extract_calls` and all 14 languages are ✓ on this axis. If future work needs distinct graph-edge types for `new Foo()` vs plain `foo()`, that's a separate spec; not in this scope.

## 10. Open questions

-   **Embedding vector storage for new langs**: Lua / Bash chunks may produce many short symbols. Should embedder skip < N tokens? Already a concern for existing langs, treat uniformly.
-   **Parity harness ground truth**: do we have upstream gitnexus indexing the same fixtures for diff comparison? `scripts/parity/run_parity.py` depends on a locally-installed `gnx` CLI; the multi-language extension `all_languages_parity.py` expects `.sample_repo/<Lang>/` dirs that aren't in the repo. Phase 5 may reduce to internal consistency checks unless we wire up upstream fixtures.
