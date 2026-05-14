# gnx-rs Parser Expansion — Fill gaps + add new languages

**Goal:** Close the 22 missing-feature cells in the current 14-language parity matrix and add 6+ new languages (starting with Lua), using parallel Sonnet subagents under an Opus-defined contract.

**Approach:** Decompose the work into three layers — (1) a small set of cross-cutting design decisions done once by Opus, (2) a Sonnet worker template for per-language pattern-fill tasks fanned out in parallel, (3) batched serial work for the parts that touch shared code (`calls.rs`).

**Date:** 2026-05-14

---

## 1. Background

All language support in `gnx-rs` is implemented via `tree-sitter` + a `queries.scm` capture file + a thin Rust parser at `crates/gnx-analyzer/src/<lang>/parser.rs`. Parser files range from 127 lines (C, the simplest) to 254 lines (TypeScript, the most complete, which includes route-handling and a dedup loop the other parsers lack — when templating a new language, prefer C or Rust as the base, not TS, to avoid copying TS-specific logic). The depth of language support is roughly proportional to the size of `queries.scm`:

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

Total: 22 missing cells across 9 languages. The Rust scaffolding around each parser is stable and uniform; expanding coverage = writing more tree-sitter queries and (for the `Constructor Inference` axis) extending the shared call resolver in `crates/gnx-analyzer/src/calls.rs`.

## 2. Gap matrix (the 22 cells)

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
| C++ | Constructor Inference | Touches `calls.rs` — see §5 |
| Dart | Named Bindings | `import 'x.dart' show A, B` / `hide` — `show_combinator` / `hide_combinator` captures |
| Dart | Config | Register `pubspec.yaml` (shared) |
| Java | Constructor Inference | Touches `calls.rs` |
| Kotlin | Constructor Inference | Touches `calls.rs` |
| Rust | Constructor Inference | Touches `calls.rs` |
| Ruby | Constructor Inference | Touches `calls.rs` |
| C | Constructor Inference | Touches `calls.rs` |
| C++ | Constructor Inference | Touches `calls.rs` (overlaps with above) |
| Dart | Constructor Inference | Touches `calls.rs` |

Two classes:
-   **Query-only** (~16 items): pure `queries.scm` edits, no Rust changes. Trivially parallelizable.
-   **Shared-code** (Config: 6 langs, Constructor Inference: 7 langs): touches `calls.rs` or new shared `config_detector` — must be serialized or contract-first refactored.

## 3. Parallelization strategy

### 3.1 Cost rationale

| | Sonnet 4.6 | Opus 4.7 |
|---|---:|---:|
| Input $/M | $3 | $15 |
| Output $/M | $15 | $75 |
| Est. cost per language task | ~$0.17 | ~$0.83 |
| 28 tasks total (22 gaps + Lua + 5 new) | **~$5** | **~$23** |

Tree-sitter query writing is high-template, low-novel-reasoning work: Sonnet is the right default. Reserve Opus for the cross-cutting design pass that comes before the fan-out.

### 3.2 Subagent isolation considerations

Each subagent is a fresh context — it must re-read sample parsers, `queries.scm` templates, and the target grammar's `node-types.json`. To avoid 28× duplicated reads:

-   **Shared brief**: write a single `docs/superpowers/plans/parser-worker-brief.md` containing (a) the parser template anatomy, (b) capture naming conventions, (c) verification commands. Each subagent's prompt opens with "read this brief, then [task-specific instructions]" — the brief's content becomes a prompt-cache hit if subagents fire within the 5-minute window.
-   **No shared mutable state during fan-out**: each subagent works on one `crates/gnx-analyzer/src/<lang>/queries.scm` and nothing else.

### 3.3 Batch table

| Phase | Tasks | Model | Concurrency | Duration |
|---|---|---|---|---|
| **0. Contract design** | Define (a) `config_detector` trait, (b) `calls.rs` ConstructorCall variant, (c) worker brief, (d) verification harness | Opus 1× | serial | ~2 hr |
| **1. Query-only gaps** | 16 cells across 9 langs | Sonnet | 8 in parallel × 2 waves | ~1 day |
| **2. Config detector wiring** | Register `Cargo.toml` / `pom.xml` / `build.gradle*` / `Gemfile` / `Makefile` / `pubspec.yaml` — 6 entries, one shared module | Sonnet 1× | serial | ~1 hr |
| **3. New language: Lua** | New crate dir + queries.scm + fixture | Sonnet 1× | independent of phase 1 | ~half day |
| **4. New languages: Bash / SQL / HCL / Scala / Elixir** | Same template, 5 langs | Sonnet | 5 in parallel | ~1 day |
| **5. Constructor Inference** | 7 langs, all writing to refactored `calls.rs` extension point | Sonnet | serial or 2-batch (after §5 refactor) | ~1.5 day |
| **6. Parity validation** | Run parity harness, file-by-file diff against upstream gitnexus output | Sonnet 1× | serial | ~half day |

Total wall-clock: **~4 days** with parallelization vs ~10 days serial. Total est. cost: **$5–$8**.

## 4. Worker brief contents

The shared `docs/superpowers/plans/parser-worker-brief.md` (to be written in Phase 0) must include:

1.  **Anatomy of a parser**: walk through `c/parser.rs` (smallest reference, 127 lines) — `Provider::new` loads grammar + query, `parse_file` runs query, captures iterated, `RawNode` / `RawImport` emitted into `LocalGraph`.
2.  **Anatomy of `queries.scm`**: capture naming convention (`@function.name`, `@function`, `@import.source`, `@import`, `@const.name`, `@struct.name`, `@struct`, `@export`, `@heritage`, `@type`, `@decorator`). Reference: TS as the gold standard.
3.  **Adding a new capture name**: corresponding handler in `parser.rs` `capture_index_for_name(...)` block + `RawNode` field mapping.
4.  **Verification recipe**:
    ```bash
    cargo build -p gnx-analyzer
    cargo test -p gnx-analyzer --test ast_test   # or per-lang fixture
    gnx analyze --repo tests/parity/fixtures/<lang>/sample_project
    gnx context --repo tests/parity/fixtures/<lang>/sample_project --name <known_symbol>
    ```
5.  **Failure mode catalogue**: wrong node name (`class_declaration` vs `class_definition`), capture index `None` returned (capture present in `.scm` but not in match — usually optional `?` issue), tree-sitter version mismatch (some grammars on `0.20` API).
6.  **Hard constraint**: the worker MUST run `cargo build` and at least one query against a fixture, and report stdout, before declaring done. No "looks correct" claims.

## 5. The `calls.rs` refactor (Phase 0 deliverable)

Current shape: every parser calls `extract_calls(tree, source)` which returns generic call edges. Constructor calls (`new Foo()`, `Foo()` returning instance, `make<Foo>` etc.) are not distinguished.

Proposed refactor:

```rust
// crates/gnx-analyzer/src/calls.rs
pub enum CallKind {
    Plain,
    Constructor,
    MethodOverride,
}

pub struct RawCall {
    pub caller_name: String,
    pub callee_name: String,
    pub kind: CallKind,
    pub line: u32,
}

// Per-language extractor signature
pub trait CallExtractor {
    fn extract(&self, tree: &Tree, source: &[u8]) -> Vec<RawCall>;
}
```

After the refactor, the 7 Constructor Inference workers each write a per-language `CallExtractor` impl with `CallKind::Constructor` detection, in isolation. No more conflicts on `calls.rs`.

The refactor itself (Phase 0) should be done once by a single agent (Opus or careful Sonnet) — touches all 14 existing parsers' call sites but in a uniform way.

## 6. Lua addition — concrete spec

### 6.1 Dependencies

```toml
# crates/gnx-analyzer/Cargo.toml
tree-sitter-lua = "0.4"  # pin once selected
```

### 6.2 Files to create

```
crates/gnx-analyzer/src/lua/
├── mod.rs           # pub mod parser;
├── parser.rs        # ~180 lines, copy c/parser.rs and adapt
└── queries.scm      # ~70 lines, draft below
```

### 6.3 `queries.scm` draft

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

### 6.4 Parser registration

Two sites in `crates/gnx-cli/src/commands/analyze.rs` must be updated together:

```rust
// 1. Extension routing — extend the `match ext` arm that picks a provider:
match ext {
    "lua" | "luau" => /* route to lua provider */,
    // ... existing arms ...
}

// 2. Provider initialization — add a register_provider call alongside the others:
pipeline.register_provider(Box::new(
    gnx_analyzer::lua::parser::LuaProvider::new()?
));
```

Both edits live in the same file; do them together to avoid the common pitfall of registering the provider but forgetting the extension routing (the provider then silently never runs on `.lua` files).

Also: `crates/gnx-analyzer/src/lib.rs` needs `pub mod lua;`.

### 6.5 Fixture

```
tests/parity/fixtures/lua/sample_project/
├── init.lua              # uses require, defines functions
├── module_a.lua          # exports a table with methods
└── README.md
```

Path convention matches the existing `tests/parity/fixtures/basic/` (TypeScript). Each new language gets a sibling directory under `tests/parity/fixtures/`.

### 6.6 Expected coverage

| Cell | Status | Notes |
|---|---|---|
| Imports | ✓ | `require("mod")` |
| Named Bindings | — | Lua's `require` returns the table; named binding is `local X = require("...")` — could capture |
| Exports | — | Module-level `return { ... }` is the convention; non-trivial heuristic |
| Heritage | — | `setmetatable(Child, { __index = Parent })` — pattern-detectable, future work |
| Type Annotations | — | Vanilla Lua untyped; Luau has annotations (future) |
| Constructor Inference | ~ | `setmetatable({}, Class)` pattern — punted to Phase 5 |
| Config | ✓ | `.luarc.json`, `init.lua` as entry |
| Frameworks | ~ | LÖVE (`love.load`/`love.update`), Neovim plugin (`require('plenary')`) — future |
| Entry Points | ✓ | Top-level `function main` or returned module |

Expected initial parity: **5/9 columns**, comparable to Ruby's coverage.

## 7. Additional languages (Phase 4)

| Language | tree-sitter crate | Initial scope | Effort |
|---|---|---|---|
| **Bash** | `tree-sitter-bash` | functions, `source`/`.` imports, env vars as const | ½ day |
| **SQL** | `tree-sitter-sql` | `CREATE FUNCTION` / `CREATE VIEW` / `CREATE PROCEDURE` as defines; table refs as imports | ½ day |
| **HCL / Terraform** | `tree-sitter-hcl` | `resource` / `module` / `data` blocks as classes; module imports | ½ day |
| **Scala** | `tree-sitter-scala` | full OO + functional — most complete coverage achievable | 1 day |
| **Elixir** | `tree-sitter-elixir` | `defmodule` / `def` / `defp`, `alias` / `import` / `use` | 1 day |

Each goes through the same template as Lua. Bash + SQL + HCL group is particularly valuable because **they're often cross-file targets from existing languages** (Python calling shell, application code embedding SQL strings, Terraform consumed by CI/CD scripts) — adding them creates new edges in repos that already have multiple languages indexed.

## 8. Build sequence

```
Day 1 (Opus, serial)
  └─ Phase 0: contract design
     ├─ docs/superpowers/plans/parser-worker-brief.md
     ├─ calls.rs refactor (CallKind, RawCall, CallExtractor trait)
     ├─ config_detector trait + initial registry
     └─ verification harness command

Day 2 (Sonnet, parallel × 8)
  └─ Phase 1: 16 query-only cells, wave 1 (8 tasks)
Day 2 PM (Sonnet, parallel × 8)
  └─ Phase 1: 16 query-only cells, wave 2 (8 tasks)
Day 2 PM (Sonnet, serial)
  └─ Phase 2: config detector wirings (6 entries)

Day 3 (Sonnet, parallel × 6)
  └─ Phase 3+4: Lua + Bash + SQL + HCL + Scala + Elixir

Day 4 (Sonnet, serial or 2-batch)
  └─ Phase 5: Constructor Inference per-lang implementations
Day 4 PM (Sonnet, serial)
  └─ Phase 6: parity validation + cleanup
```

## 9. Dispatch checklist

Before Phase 1 fan-out, verify:

-   [ ] Worker brief written and validated against one trial task
-   [ ] `calls.rs` refactor merged
-   [ ] `config_detector` trait + registry merged
-   [ ] One trial subagent run completed successfully (e.g. JS Type Annotation → confirm flow works end-to-end including `cargo build` + fixture query)
-   [ ] Verification harness command works (no false negatives)
-   [ ] At-most-N concurrent agents decided (recommend 6–8 to stay under rate limits)

## 10. Non-goals

-   **Framework-specific extraction** (React component graph, Spring beans, Rails routes) — separate spec; can layer on once base parsers are solid.
-   **Type inference across files** — keep type captures local; full type resolution is `resolution/` crate's concern, not the per-language parser's.
-   **Refactoring tools per language** — `rename`-style operations stay generic for now.
-   **LSP integration** — out of scope; gnx-rs is an indexer, not a language server.

## 11. Open questions

-   **Constructor Inference scope**: for languages where construction is ambiguous (Python `Foo()` could be a function or a class call), do we require resolving the call target first, or emit a `CallKind::ConstructorMaybe` and let the resolution pass decide? Recommendation: latter, keep parsers local.
-   **Embedding vector storage for new langs**: Lua / Bash chunks may produce many short symbols. Should embedder skip < N tokens? Already a concern for existing langs, treat uniformly.
-   **Parity harness ground truth**: do we have upstream gitnexus indexing the same fixtures for diff comparison? If not, Phase 6 reduces to internal consistency checks.
