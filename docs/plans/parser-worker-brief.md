# Parser Worker Brief

**Audience:** A Sonnet subagent dispatched to add one new language to `cgn-analyzer`.
**Goal:** Produce a working `LanguageProvider` for one language: parser file, queries.scm, fixture, and all registration sites updated — with `cargo build` passing and one fixture query returning a non-empty result.

Read this brief in full before reading per-language instructions in the dispatch prompt.

---

## 1. Anatomy of a parser

Every language has the same shape in `crates/cgn-analyzer/src/<lang>/`:

```
<lang>/
├── mod.rs        # `pub mod parser;`
├── parser.rs     # ~150–200 lines, implements LanguageProvider
└── queries.scm   # tree-sitter capture file
```

Reference templates (read these before writing):
- `crates/cgn-analyzer/src/c/parser.rs` — 127 lines, **smallest and cleanest**, use this as your starting template
- `crates/cgn-analyzer/src/rust/parser.rs` — 183 lines, similar simplicity
- **Do NOT** template from `crates/cgn-analyzer/src/typescript/parser.rs` — it has a route-dedup loop only TS needs

`parser.rs` structure (from c/parser.rs):

```rust
use crate::calls::extract_calls;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use cgn_core::graph::NodeKind;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

pub struct XxxProvider {
    query: Query,
}

impl XxxProvider {
    pub fn new() -> anyhow::Result<Self> {
        let language = tree_sitter_xxx::LANGUAGE.into();
        let query_source = include_str!("queries.scm");
        let query = Query::new(&language, query_source)?;
        Ok(Self { query })
    }
}

impl LanguageProvider for XxxProvider {
    fn name(&self) -> &'static str { "xxx" }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        let language = tree_sitter_xxx::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language)?;
        let tree = parser.parse(source, None).ok_or_else(|| anyhow::anyhow!("parse fail"))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source);

        let mut nodes = Vec::new();
        let mut imports = Vec::new();

        // Resolve capture indices once before the loop:
        let idx_function_name = self.query.capture_index_for_name("function.name");
        let idx_function       = self.query.capture_index_for_name("function");
        // ... add one pair (name + body) per capture you defined
        let idx_import_source  = self.query.capture_index_for_name("import.source");

        while let Some(m) = matches.next() {
            // walk m.captures; build RawNode { uid, name, kind, file_idx, span }
            // and RawImport { source, target_name, line }
        }

        let calls = extract_calls(tree.root_node(), source, &nodes, &["call_expression"]);
        Ok(LocalGraph { nodes, imports, calls, file_path: path.to_path_buf() })
    }
}
```

The exact loop body — see c/parser.rs lines 38–95.

## 2. Capture name convention

**Use the `@X.name` convention for new languages** (matches TypeScript / Rust / Python / Java / 11 others).

The ONE exception in the codebase is Swift, which historically uses `@name.X` (e.g. `@name.class`). This was a legacy choice; do not mimic it for new languages. Always use `@class.name`, `@function.name`, `@method.name`, etc.

Standard captures (use whichever apply to your language):

| Capture | Purpose |
|---|---|
| `@function.name` | function/procedure name identifier |
| `@function` | entire function definition span |
| `@class.name` | class/struct/interface name identifier |
| `@class` | entire class definition span |
| `@struct.name` / `@struct` | struct (if distinct from class — e.g. Rust) |
| `@method.name` / `@method` | method on a class (if distinct) |
| `@const.name` / `@const` | top-level const or var |
| `@import.source` | the imported path/module name (string content) |
| `@import` | entire import statement |
| `@export` | export wrapper around a function/class |
| `@heritage` | extends/implements/inherits target |
| `@type` | type annotation on parameter or return |
| `@decorator` | annotation/decorator on declaration |

Match the capture name **exactly** in `capture_index_for_name(...)` — `capture_index_for_name("function.name")` for `@function.name`. Underscore vs dot mismatches silently return `None` and produce empty parses.

## 3. NodeKind mapping

`cgn_core::graph::NodeKind` variants you'll use:
- `Function` — top-level function, free function
- `Method` — method on a class/struct
- `Class` — class, interface, struct (most languages — your choice for "class-like")
- `Const` — top-level constant or variable
- `Property` — class field/property (rare to capture)
- `Route` — HTTP/web route (skip unless your language is web-focused)
- `File`, `Folder` — created by the pipeline, not by the parser

Choose Class vs Struct based on language convention. Rust has both; most languages just use Class.

## 4. Wiring into the pipeline (4 sites + 1 dep)

Each new language needs **five** edits outside its own directory. The fifth (pipeline.rs) is non-obvious and **was missed by all 7 Wave 1 workers initially** — without it your provider is registered but never called.

### 4.1 `crates/cgn-analyzer/Cargo.toml`

Add one line under the `[dependencies]` section, after the existing tree-sitter language deps:

```toml
tree-sitter-<lang> = "<version>"   # use crates.io version that matches tree-sitter = "0.25"
```

If the crate is not on crates.io or is stuck on an older `tree-sitter` API, use a `{ git = "..." }` or `{ path = "../vendor/..." }` source — see `tree-sitter-kotlin` (git) and `tree-sitter-swift` (path) for precedent.

### 4.2 `crates/cgn-analyzer/src/lib.rs`

Add one line, alphabetical-ish ordering:

```rust
pub mod <lang>;
```

### 4.3 `crates/cgn-cli/src/commands/analyze.rs`

Two edits in this file:

a) **Extension routing** — extend the `match ext { ... }` arm around line 50, adding your extensions:

```rust
match ext {
    "ts" | "tsx" | "py" | ...existing... | "<your_ext>" => {
        files_to_analyze.push(...);
    }
    _ => {}
}
```

b) **Provider registration** — add one line in the `register_provider` block around line 76:

```rust
pipeline.register_provider(Box::new(<Lang>Provider::new().unwrap()));
```

**Both edits must happen together.** Registering a provider without routing the extension means the provider is loaded but never called on files. Routing an extension without registering the provider means the file is scanned but skipped silently.

### 4.4 `crates/cgn-core/src/analyzer/pipeline.rs` — **DON'T MISS THIS**

`pipeline.rs` has a `find_provider()` function with its own extension→provider match arm. Without adding your extensions here, your provider is registered but never receives files (they pass scanning but silently get no provider dispatch). Search for the existing match arm and add your extension:

```rust
// in find_provider() inside pipeline.rs
match ext {
    "ts" | "tsx" => "typescript",
    // ... existing arms ...
    "<your_ext>" => "<your_lang_name>",   // ← add this
    _ => return None,
}
```

The string on the right must match the value returned by your `LanguageProvider::name()` method.

For languages with no extension (e.g. `Dockerfile`), you'll need to extend the dispatch to also check filename basename — see how Wave 1's Dockerfile worker handled it.

## 5. Fixture

Create a fixture directory at:

```
tests/parity/fixtures/<lang>/sample_project/
├── README.md      # one paragraph: what's in this fixture, what symbols to expect
├── main.<ext>     # one file exercising basic syntax
└── ...optional more files demonstrating import/heritage/etc.
```

Path convention is `tests/parity/fixtures/<lang>/sample_project/` — sibling to the existing `tests/parity/fixtures/basic/` (TypeScript). The fixture should be **minimal but realistic** — enough to verify your parser extracts non-trivial symbols, not a full real-world repo.

Aim for 30–80 lines total across all fixture source files. Include:
- At least one function/class declaration
- At least one import/require/include statement (if the language has them)
- One inheritance/heritage relation (if applicable)
- One call from one declared function to another

## 6. Verification recipe (hard requirement)

You **must** run these commands in your worktree before declaring done. Do not declare success based on visual code inspection. Report the actual stdout in your final response.

```bash
# 1. Build must pass cleanly
cargo build -p cgn-analyzer 2>&1 | tail -20
cargo build -p cgn-cli 2>&1 | tail -20

# 2. Indexing must succeed (produce a non-empty graph.bin)
target/debug/cgn-cli analyze --repo tests/parity/fixtures/<lang>/sample_project
ls -la tests/parity/fixtures/<lang>/sample_project/.cgn/graph.bin

# 3. One symbol must resolve
target/debug/cgn-cli context \
  --repo tests/parity/fixtures/<lang>/sample_project \
  --name <one_function_from_your_fixture>
# Should print JSON with status: found, not error
```

If `cargo build` fails, fix it before continuing. Do not commit a build-broken state. Common fix: capture-index-for-name string didn't match the capture you wrote in queries.scm.

## 7. Failure mode catalogue

| Symptom | Likely cause | Fix |
|---|---|---|
| `cargo build` fails with "no `LANGUAGE` in tree_sitter_X" | The crate uses old `language()` fn API, not the `LANGUAGE` constant | Use `tree_sitter_xxx::language()` and call `.into()` on the result, or pin to a newer version |
| `cargo build` succeeds but `admin index` produces empty graph for your fixture | queries.scm capture names don't match `capture_index_for_name` strings | Check both sides — they must be byte-identical (no underscore vs dot mixups) |
| `cgn inspect X` returns "not found" | The fixture file's extension isn't in `match ext` arm OR the provider isn't in the register block | Re-check `index.rs` two-site edit |
| Parser panics during `tree-sitter::Parser::set_language` | tree-sitter version mismatch (your grammar crate was built against 0.20, repo uses 0.25) | Find a fork or version on crates.io built against tree-sitter 0.25 |
| `cursor.matches` returns nothing despite valid syntax | Top-level `(_)` wildcards in queries.scm can match invisibly; capture-name unmatched in target grammar's node names | Run `tree-sitter parse <file>` against the grammar to see actual node names |

## 8. Hard constraints

1. **You must `cargo build -p cgn-analyzer` and `cargo build -p cgn-cli` and both must succeed before declaring done.** Report the last 20 lines of each build's output in your response.
2. **You must run `cgn-cli analyze` on your fixture and confirm `.cgn/graph.bin` exists.** Report `ls -la` of the output.
3. **You must run `cgn-cli context --name <some_symbol>` on your fixture and get a non-error response.** Report the JSON output.
4. **You must commit your changes** on the worktree branch with a clear commit message like `feat(analyzer): add <Lang>Provider`.
5. **You must not modify existing language parsers.** Only your new `<lang>/` directory + the 4 wiring sites (Cargo.toml, lib.rs, analyze.rs ×2).
6. **If something blocks you that's not in this brief**, stop and explain rather than improvising. Do not write speculative code.
