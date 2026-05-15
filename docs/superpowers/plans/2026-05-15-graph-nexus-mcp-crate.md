# graph-nexus-mcp Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `gnx mcp serve` as a working MCP server (stdio JSON-RPC) that exposes the 8 core gnx commands as auto-discovered MCP tools, with dual dispatch modes (spawn default, daemon opt-in).

**Architecture:** New `graph-nexus-mcp` crate provides the server library. The existing `graph-nexus-cli` adds a `gnx mcp` subcommand wrapping it. Tool discovery is fully dynamic via the `inventory` crate — each `commands/*.rs` opts in by adding one `gnx_register_mcp_tool!` macro line. Daemon mode loads `Engine` once and refreshes via mtime-remap; spawn mode `Command::spawn`s a fresh `gnx <subcmd>` per call.

**Tech Stack:** Rust 1.85+, `rmcp` (official MCP SDK), `inventory` (link-time registration), `schemars` (JsonSchema derive), `serde_json` (already in workspace), `tempfile` (for tests). Existing `tokio` runtime in CLI.

**Spec:** `docs/specs/2026-05-15-host-integration.md` (relevant sections: "Single-binary model", "MCP crate infrastructure", "Per-command self-registration", "json_to_argv").

**Out of scope** (will be planned separately):
- `gnx admin` TUI shell (subproject B)
- TUI MCP install handler (subproject C)
- Codex CLI fork patch (subproject D)
- Gemini CLI fork patch (subproject E)

This plan only ships the MCP server itself. Host registration is done manually by the user editing their `.mcp.json` until subproject C lands.

---

## File Structure

```
crates/graph-nexus-mcp/                     ← NEW CRATE
├── Cargo.toml
├── src/
│   ├── lib.rs                              ← module re-exports
│   ├── registry.rs                         ← GnxMcpTool struct + inventory collect + name derivation
│   ├── argv.rs                             ← json_to_argv conversion
│   ├── spawn.rs                            ← spawn-mode handler
│   ├── daemon.rs                           ← daemon-mode handler + ensure_fresh (mtime-remap)
│   └── server.rs                           ← stdio JSON-RPC loop wiring
└── tests/
    ├── argv_test.rs                        ← json_to_argv unit tests
    ├── registry_test.rs                    ← inventory discovery + name derivation
    └── server_e2e.rs                       ← full server e2e in both modes

crates/graph-nexus-cli/
├── src/
│   ├── output.rs                           ← MODIFY: add emit_to_string()
│   ├── commands/
│   │   ├── mod.rs                          ← MODIFY: pub mod mcp;
│   │   ├── mcp.rs                          ← NEW: gnx mcp serve/tools subcommand
│   │   ├── context.rs                      ← MODIFY: derives + run_inner + macro line
│   │   ├── impact.rs                       ← MODIFY: same
│   │   ├── query.rs                        ← MODIFY: same
│   │   ├── detect_changes.rs               ← MODIFY: same
│   │   ├── rename.rs                       ← MODIFY: same
│   │   ├── route_map.rs                    ← MODIFY: same
│   │   ├── shape_check.rs                  ← MODIFY: same
│   │   └── multi_query.rs                  ← MODIFY: same
│   └── main.rs                             ← MODIFY: add Mcp variant + dispatch arms
└── Cargo.toml                              ← MODIFY: add graph-nexus-mcp + schemars deps
```

---

## Phase 1 — Foundations

### Task 1: Output helper — extract `emit_to_string`

**Files:**
- Modify: `crates/graph-nexus-cli/src/output.rs`

- [ ] **Step 1: Write the failing test**

Add to bottom of `crates/graph-nexus-cli/src/output.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emit_to_string_json_returns_serialized_value() {
        let value = json!({"status": "success", "results": []});
        let out = emit_to_string(&value, OutputFormat::Json).expect("ok");
        assert!(out.contains("\"status\":\"success\""));
        assert!(out.contains("\"results\":[]"));
        // No trailing newline — caller is responsible for println! if they want stdout.
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn emit_to_string_text_extracts_results_array_lines() {
        let value = json!({"results": ["line one", "line two"]});
        let out = emit_to_string(&value, OutputFormat::Text).expect("ok");
        assert_eq!(out, "line one\nline two");
    }

    #[test]
    fn emit_to_string_toon_produces_encoded_output() {
        let value = json!({"k": "v"});
        let out = emit_to_string(&value, OutputFormat::Toon).expect("ok");
        // TOON output is non-empty and not raw JSON.
        assert!(!out.is_empty());
        assert!(!out.starts_with('{'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p graph-nexus --lib output::tests
```

Expected: FAIL with `cannot find function emit_to_string in this scope`.

- [ ] **Step 3: Implement `emit_to_string`**

Replace the existing `emit` function in `crates/graph-nexus-cli/src/output.rs` with the pair:

```rust
/// Format `value` per `format` and return the rendered string. Does NOT
/// write to stdout — callers decide. Used by CLI `emit()` (which prints)
/// and by MCP daemon-mode dispatch (which wraps the string in
/// `ToolResult::text`).
pub fn emit_to_string(value: &Value, format: OutputFormat) -> Result<String, GnxError> {
    match format {
        OutputFormat::Toon => {
            let bytes = serde_json::to_vec(value)
                .map_err(|e| GnxError::Output(format!("json serialize: {e}")))?;
            _etoon::toon::encode(&bytes)
                .map_err(|e| GnxError::Output(format!("toon encode: {e}")))
        }
        OutputFormat::Json => serde_json::to_string(value)
            .map_err(|e| GnxError::Output(format!("json serialize: {e}"))),
        OutputFormat::Text => {
            if let Some(results) = value.get("results").and_then(|v| v.as_array()) {
                Ok(results
                    .iter()
                    .filter_map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"))
            } else {
                serde_json::to_string_pretty(value)
                    .map_err(|e| GnxError::Output(format!("json pretty: {e}")))
            }
        }
    }
}

/// Print `value` to stdout in the requested format. Thin wrapper over
/// [`emit_to_string`].
pub fn emit(value: &Value, format: OutputFormat) -> Result<(), GnxError> {
    println!("{}", emit_to_string(value, format)?);
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p graph-nexus --lib output::tests
```

Expected: 3 tests pass.

```bash
cargo test -p graph-nexus
```

Expected: full CLI suite passes with no regressions (all existing commands still print via `emit`).

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/output.rs
git commit -m "refactor(output): extract emit_to_string from emit

Pure splitting refactor — emit() becomes println! + emit_to_string().
Sets up shared serialization path for MCP daemon-mode dispatch (which
needs the String, not stdout side effect)."
```

---

### Task 2: Create `graph-nexus-mcp` crate scaffold

**Files:**
- Create: `crates/graph-nexus-mcp/Cargo.toml`
- Create: `crates/graph-nexus-mcp/src/lib.rs`
- Modify: `Cargo.toml` (workspace root — add to `members`)
- Modify: `crates/graph-nexus-cli/Cargo.toml` (add `schemars` dep — needed by upcoming command refactors)

- [ ] **Step 1: Write `Cargo.toml`**

```toml
# crates/graph-nexus-mcp/Cargo.toml
[package]
name = "graph-nexus-mcp"
version = "0.1.0"
edition = "2024"
license = "PolyForm-Noncommercial-1.0.0"

[dependencies]
graph-nexus-core = { path = "../graph-nexus-core" }
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
schemars = "0.8"
inventory = "0.3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "io-std", "process"] }
rmcp = { version = "0.2", features = ["server", "transport-io"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write `lib.rs` placeholder**

```rust
// crates/graph-nexus-mcp/src/lib.rs
//! MCP server library backing `gnx mcp serve`. Built around `inventory`
//! for zero-hardcode tool discovery; each gnx CLI command opts in via
//! `gnx_register_mcp_tool!`.

pub mod argv;
pub mod daemon;
pub mod registry;
pub mod server;
pub mod spawn;
```

(The four `pub mod` files will be created empty for now; later tasks fill them.)

```bash
mkdir -p crates/graph-nexus-mcp/src crates/graph-nexus-mcp/tests
touch crates/graph-nexus-mcp/src/argv.rs
touch crates/graph-nexus-mcp/src/daemon.rs
touch crates/graph-nexus-mcp/src/registry.rs
touch crates/graph-nexus-mcp/src/server.rs
touch crates/graph-nexus-mcp/src/spawn.rs
```

- [ ] **Step 3: Wire into workspace**

Edit `Cargo.toml` at repo root, add to `[workspace] members`:

```toml
members = [
    "crates/graph-nexus-core",
    "crates/graph-nexus-analyzer",
    "crates/graph-nexus-cli",
    "crates/graph-nexus-mcp",   # ← new line
]
```

Edit `crates/graph-nexus-cli/Cargo.toml` `[dependencies]`:

```toml
schemars = "0.8"
graph-nexus-mcp = { path = "../graph-nexus-mcp" }
```

- [ ] **Step 4: Verify it builds**

```bash
cargo build -p graph-nexus-mcp
```

Expected: clean build with empty modules.

```bash
cargo build -p graph-nexus
```

Expected: CLI still builds (no actual usage of graph-nexus-mcp yet).

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp Cargo.toml crates/graph-nexus-cli/Cargo.toml
git commit -m "feat(mcp): scaffold graph-nexus-mcp crate

Empty crate with module skeleton (registry / argv / spawn / daemon /
server). Workspace + CLI deps wired so subsequent tasks can land
incrementally. No behaviour yet."
```

---

### Task 3: Registry — `GnxMcpTool` struct + inventory collect + name derivation

**Files:**
- Modify: `crates/graph-nexus-mcp/src/registry.rs`
- Create: `crates/graph-nexus-mcp/tests/registry_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/graph-nexus-mcp/tests/registry_test.rs
use graph_nexus_mcp::registry::{derive_subcommand, derive_tool_name};

#[test]
fn derive_tool_name_extracts_last_segment_with_gnx_prefix() {
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::context"), "gnx_context");
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::detect_changes"), "gnx_detect_changes");
    assert_eq!(derive_tool_name("graph_nexus_cli::commands::multi_query"), "gnx_multi_query");
}

#[test]
fn derive_subcommand_returns_last_segment_raw() {
    assert_eq!(derive_subcommand("graph_nexus_cli::commands::context"), "context");
    assert_eq!(derive_subcommand("graph_nexus_cli::commands::detect_changes"), "detect_changes");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p graph-nexus-mcp --test registry_test
```

Expected: FAIL with `unresolved import graph_nexus_mcp::registry::derive_tool_name`.

- [ ] **Step 3: Implement registry.rs**

```rust
// crates/graph-nexus-mcp/src/registry.rs
//! Tool registry: types, inventory collection, name-derivation helpers.
//!
//! Each registered tool carries everything BOTH dispatch modes need:
//! - `handler` — daemon mode in-process call signature
//! - `subcommand` — spawn mode subprocess argument
//! - `name` / `description` / `schema` — MCP protocol metadata
//!
//! All four are filled by the `gnx_register_mcp_tool!` macro (Task 11).
//! At runtime, the MCP server iterates `inventory::iter::<GnxMcpTool>()`
//! and registers each.

use graph_nexus_core::GnxError;
use schemars::schema::RootSchema;
use serde_json::Value;

/// Engine handle abstracted at the boundary so this crate doesn't pull
/// the whole `graph-nexus-cli` Engine type into its public API.
/// Daemon mode wires this in `daemon.rs`; spawn mode never uses it.
pub trait EngineRef: Send + Sync {
    /// Path of the graph.bin currently loaded (for mtime-remap).
    fn graph_path(&self) -> &std::path::Path;
}

pub struct GnxMcpTool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> RootSchema,
    /// Daemon mode: in-process handler.
    pub handler: fn(Value, &dyn EngineRef) -> Result<Value, GnxError>,
    /// Spawn mode: subcommand to pass to `Command::new(self_exe).arg(_)`.
    pub subcommand: &'static str,
}

inventory::collect!(GnxMcpTool);

/// Strip the leading `graph_nexus_cli::commands::` (or any prefix) and
/// prepend `gnx_`. The last `::` segment IS the subcommand identifier
/// in snake_case, which matches both the CLI subcommand name and the
/// desired MCP tool name (with prefix).
pub fn derive_tool_name(module_path: &str) -> &'static str {
    let last = module_path.rsplit("::").next().unwrap_or(module_path);
    // Leak to 'static — module_path is itself 'static so this is sound.
    // We can't avoid the allocation entirely because we need a
    // formatted string, but each command-file's call only ever yields
    // one allocation for the binary's lifetime.
    Box::leak(format!("gnx_{last}").into_boxed_str())
}

pub fn derive_subcommand(module_path: &str) -> &'static str {
    module_path.rsplit("::").next().unwrap_or(module_path)
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p graph-nexus-mcp --test registry_test
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/registry.rs crates/graph-nexus-mcp/tests/registry_test.rs
git commit -m "feat(mcp): GnxMcpTool registry + name derivation helpers

inventory::collect! makes GnxMcpTool collectable at link time.
derive_tool_name / derive_subcommand extract the subcommand identifier
from module_path!() so the macro (Task 11) needs no string literals."
```

---

### Task 4: `json_to_argv` conversion (spawn-mode helper)

**Files:**
- Modify: `crates/graph-nexus-mcp/src/argv.rs`
- Create: `crates/graph-nexus-mcp/tests/argv_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/graph-nexus-mcp/tests/argv_test.rs
use graph_nexus_mcp::argv::json_to_argv;
use serde_json::json;

#[test]
fn flat_string_args_become_double_dashed_flags() {
    let argv = json_to_argv(&json!({"name": "validateUser", "format": "json"})).unwrap();
    // Order isn't guaranteed by serde_json::Map iteration, so check membership.
    assert!(argv.windows(2).any(|w| w == ["--name", "validateUser"]));
    assert!(argv.windows(2).any(|w| w == ["--format", "json"]));
    assert_eq!(argv.len(), 4);
}

#[test]
fn bool_true_becomes_flag_only() {
    let argv = json_to_argv(&json!({"includeTests": true})).unwrap();
    assert_eq!(argv, vec!["--include-tests"]);
}

#[test]
fn bool_false_emits_nothing() {
    let argv = json_to_argv(&json!({"includeTests": false})).unwrap();
    assert!(argv.is_empty());
}

#[test]
fn null_values_are_skipped() {
    let argv = json_to_argv(&json!({"name": "foo", "uid": null})).unwrap();
    assert_eq!(argv, vec!["--name", "foo"]);
}

#[test]
fn numbers_serialize_as_strings() {
    let argv = json_to_argv(&json!({"limit": 42, "ratio": 3.14})).unwrap();
    assert!(argv.windows(2).any(|w| w == ["--limit", "42"]));
    assert!(argv.windows(2).any(|w| w == ["--ratio", "3.14"]));
}

#[test]
fn camel_case_keys_get_kebab_case_flags() {
    let argv = json_to_argv(&json!({"baseRef": "main"})).unwrap();
    assert_eq!(argv, vec!["--base-ref", "main"]);
}

#[test]
fn non_object_root_errors() {
    let res = json_to_argv(&json!([1, 2, 3]));
    assert!(res.is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p graph-nexus-mcp --test argv_test
```

Expected: FAIL with `unresolved import graph_nexus_mcp::argv::json_to_argv`.

- [ ] **Step 3: Implement `json_to_argv`**

```rust
// crates/graph-nexus-mcp/src/argv.rs
//! Convert MCP-side JSON args into the clap CLI flag form gnx
//! subcommands expect.
//!
//! Used by spawn-mode dispatch. Daemon mode never goes through this —
//! it passes the JSON straight to each command's `run_inner` which
//! takes its already-typed `Args` struct via serde_json::from_value.

use anyhow::{bail, Result};
use serde_json::Value;

/// Map camelCase → kebab-case for clap long flag form. clap by default
/// converts the Rust field name `include_tests` to `--include-tests`,
/// so JSON callers using `includeTests` need this translation.
fn to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    out
}

pub fn json_to_argv(args: &Value) -> Result<Vec<String>> {
    let Value::Object(map) = args else {
        bail!("expected JSON object at args root, got {}", type_name(args));
    };
    let mut out = Vec::with_capacity(map.len() * 2);
    for (k, v) in map {
        let flag = format!("--{}", to_kebab(k));
        match v {
            Value::Null => continue,
            Value::Bool(true) => out.push(flag),
            Value::Bool(false) => continue,
            Value::String(s) => {
                out.push(flag);
                out.push(s.clone());
            }
            Value::Number(n) => {
                out.push(flag);
                out.push(n.to_string());
            }
            Value::Array(_) | Value::Object(_) => {
                bail!("nested array/object args not supported (key={k}); flatten or use daemon mode");
            }
        }
    }
    Ok(out)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p graph-nexus-mcp --test argv_test
```

Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/argv.rs crates/graph-nexus-mcp/tests/argv_test.rs
git commit -m "feat(mcp): json_to_argv conversion for spawn-mode dispatch

Translates {camelCase:value} JSON into clap --kebab-case form. Handles
flag bools (true→present / false→omit), null skip, number stringify,
errors on nested objects/arrays (MVP scope — gnx args are flat)."
```

---

## Phase 2 — Per-command MCP enablement

Each of the 8 in-scope commands gets the same 4-part refactor:
1. Add `Serialize, Deserialize, JsonSchema` derives to its `Args` struct
2. Split `run` into `run_inner` (returns `Value`) + `run` (calls inner + `emit`)
3. Add doc comments on Args + fields for schemars to lift into MCP description
4. Add `gnx_register_mcp_tool!` macro line at module bottom

The macro itself is defined in Task 11 (after the command refactors stabilize the pattern).

Tasks 5–9 below illustrate the pattern on context (canonical), impact, query, detect_changes, route_map. Tasks 10–12 inline the same pattern to the remaining three commands (rename, shape_check, multi_query).

### Task 5: Refactor `commands/context.rs` (canonical pattern)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/context.rs`

- [ ] **Step 1: Read the current shape**

```bash
head -40 crates/graph-nexus-cli/src/commands/context.rs
```

Note the existing `ContextArgs` struct fields and the `run` function signature. The refactor must preserve both for CLI callers.

- [ ] **Step 2: Add the test for `run_inner` returning Value**

Append to the bottom of `crates/graph-nexus-cli/src/commands/context.rs`:

```rust
#[cfg(test)]
mod inner_tests {
    use super::*;

    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        // We don't have a real engine in this unit test — assert the
        // signature compiles. A real e2e test runs against a built graph
        // in the integration suite.
        fn _accepts(_f: fn(ContextArgs, &crate::engine::Engine) -> Result<serde_json::Value, graph_nexus_core::GnxError>) {}
        _accepts(run_inner);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test -p graph-nexus --lib commands::context::inner_tests
```

Expected: FAIL with `cannot find function run_inner in scope`.

- [ ] **Step 4: Apply the refactor**

Top of file — add derives:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Look up a symbol's definition, callers, callees, and surrounding
/// context. Returns the symbol's metadata plus its immediate
/// upstream/downstream neighbours from the graph.
#[derive(clap::Args, Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextArgs {
    /// Symbol name (e.g. "validateUser"). Use --uid for ambiguous names.
    #[arg(long)]
    pub name: Option<String>,
    /// Unique identifier from graph (e.g. "Function:src/auth.ts:foo").
    /// Use this when --name is ambiguous; --uid wins if both are set.
    #[arg(long)]
    pub uid: Option<String>,
    // ... preserve existing fields verbatim, adding /// doc comments
    // for any field that lacks one
}
```

Split the body — the existing `run` becomes:

```rust
/// Pure business logic. Used by both CLI (`run`) and MCP daemon-mode
/// dispatch. Returns the result as structured JSON; no I/O.
pub fn run_inner(args: ContextArgs, engine: &crate::engine::Engine)
    -> Result<serde_json::Value, graph_nexus_core::GnxError>
{
    // ← move existing run() body here, but instead of calling emit() at
    //   the end, return the serde_json::Value that was being built.
    // ← if the old code mixed in println!s for text output, those
    //   become entries in a `results: Vec<String>` array on the Value;
    //   text-mode emit() already knows how to print such arrays.
}

pub fn run(args: ContextArgs, engine: &crate::engine::Engine)
    -> Result<(), graph_nexus_core::GnxError>
{
    let format = crate::output::OutputFormat::parse(args.format.as_deref());
    let value = run_inner(args, engine)?;
    crate::output::emit(&value, format)
}
```

- [ ] **Step 5: Run the test + existing CLI tests**

```bash
cargo test -p graph-nexus --lib commands::context
```

Expected: new `inner_tests::run_inner_returns_structured_value_not_unit` passes; all existing context tests still pass.

```bash
cargo build -p graph-nexus
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/context.rs
git commit -m "refactor(context): split run into run_inner + emit shell

run_inner returns serde_json::Value (pure logic, no I/O); run wraps
with output::emit for CLI. Args struct gains Serialize/Deserialize/
JsonSchema derives for upcoming MCP wrapper consumption."
```

---

### Tasks 6–10: Replay Task 5's pattern on the remaining 7 commands

Tasks 6 through 10 each apply Task 5's exact 5-step refactor — **the
only thing that varies between tasks is the file path and three
identifier names**: the file under `commands/`, the `<Cmd>Args` struct
type, and the commit message subject. **Every step's code is identical
to Task 5's** modulo the substitution table below; read Task 5 for the
canonical full pattern.

#### Substitution table

| Task | File | Args type | Commit message |
|---|---|---|---|
| 6 | `crates/graph-nexus-cli/src/commands/impact.rs` | `ImpactArgs` | `refactor(impact): split run into run_inner + emit shell` |
| 7 | `crates/graph-nexus-cli/src/commands/query.rs` | `QueryArgs` | `refactor(query): split run into run_inner + emit shell` |
| 8 | `crates/graph-nexus-cli/src/commands/detect_changes.rs` | `DetectChangesArgs` | `refactor(detect_changes): split run into run_inner + emit shell` |
| 9 | `crates/graph-nexus-cli/src/commands/route_map.rs` | `RouteMapArgs` | `refactor(route_map): split run into run_inner + emit shell` |
| 10a | `crates/graph-nexus-cli/src/commands/rename.rs` | `RenameArgs` | `refactor(rename): split run into run_inner + emit shell` |
| 10b | `crates/graph-nexus-cli/src/commands/shape_check.rs` | `ShapeCheckArgs` | `refactor(shape_check): split run into run_inner + emit shell` |
| 10c | `crates/graph-nexus-cli/src/commands/multi_query.rs` | `MultiQueryArgs` | `refactor(multi_query): split run into run_inner + emit shell` |

#### Per-task replay procedure (identical for every row above)

For each row (call its file `<FILE>`, args type `<ARGS>`, commit `<MSG>`):

- [ ] **Step 1: Add the signature-check test** — Append at file bottom:

```rust
#[cfg(test)]
mod inner_tests {
    use super::*;
    #[test]
    fn run_inner_has_value_return_type() {
        // Compile-only check on the new signature. Real behaviour is
        // covered by the command's existing integration tests.
        fn _accepts(
            _f: fn(super::<ARGS>, &crate::engine::Engine)
                -> Result<serde_json::Value, graph_nexus_core::GnxError>
        ) {}
        _accepts(run_inner);
    }
}
```

Replace `<ARGS>` with the row's Args type (e.g. `ImpactArgs`).

- [ ] **Step 2: Run + see it fail** — `cargo test -p graph-nexus --lib commands::<MODULE>::inner_tests` (e.g. `commands::impact::inner_tests`). Expected: FAIL with `cannot find function run_inner in this scope`.

- [ ] **Step 3: Apply the four-piece refactor identically to Task 5**:
  1. Add `use schemars::JsonSchema;` and `use serde::{Deserialize, Serialize};` near the top of the file (next to the existing clap `use`).
  2. Extend the existing `#[derive(...)]` line on `<ARGS>` to include `Serialize, Deserialize, JsonSchema` (preserve all existing derives like `Args`, `Debug`, `Clone`).
  3. Add a `///` doc comment immediately above the struct (1–2 sentence summary of the command — pulled from `clap`'s existing about-string if one exists), and a `///` line on every field that doesn't already have one. These doc comments become the MCP tool / arg descriptions via schemars at link time.
  4. Split the existing `run` function: rename the original body to `run_inner(args: <ARGS>, engine: &crate::engine::Engine) -> Result<serde_json::Value, graph_nexus_core::GnxError>` and change its terminal `crate::output::emit(&value, format)` to return the value (`Ok(value)`). Add a new thin `run` shell exactly like Task 5's:
     ```rust
     pub fn run(args: <ARGS>, engine: &crate::engine::Engine)
         -> Result<(), graph_nexus_core::GnxError>
     {
         let format = crate::output::OutputFormat::parse(args.format.as_deref());
         let value = run_inner(args, engine)?;
         crate::output::emit(&value, format)
     }
     ```

- [ ] **Step 4: Run all of the command's tests** — `cargo test -p graph-nexus --lib commands::<MODULE>` AND the command's integration tests (e.g. for `impact.rs` also run `cargo test -p graph-nexus --test impact_cmd` if such a file exists; check `crates/graph-nexus-cli/tests/` for matching names). Expected: pass; no regressions vs main.

- [ ] **Step 5: Commit** — single-file commit with `<MSG>` from the table:
  ```bash
  git add <FILE>
  git commit -m "<MSG>"
  ```

After Task 10c (last row) finishes: run the full suite once to catch any
cross-command regression:

```bash
cargo test -p graph-nexus
```

Expected: same pass count as main, just with the 7 new `inner_tests`
modules. If any pre-existing integration test breaks, the most likely
cause is that `run_inner` returns a `Value` shape different from what
the test expects when read back from stdout — adjust the
`run_inner` body's JSON shape to match what `emit` previously
printed before this refactor.

---

## Phase 3 — Registration macro + dispatch + server

### Task 11: Define `gnx_register_mcp_tool!` macro

**Files:**
- Create: `crates/graph-nexus-mcp/src/macros.rs`
- Modify: `crates/graph-nexus-mcp/src/lib.rs` (add `pub mod macros;`)

- [ ] **Step 1: Write a compile-test fixture**

```rust
// crates/graph-nexus-mcp/tests/macro_test.rs
//! Compile-only smoke test that the macro expands cleanly. Real
//! end-to-end registration is verified in server_e2e (Task 18).

use graph_nexus_mcp::gnx_register_mcp_tool;
use graph_nexus_mcp::registry::EngineRef;
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

/// Fixture command for macro expansion test.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DummyArgs {
    /// Some name field.
    pub name: String,
}

pub fn run_inner(_args: DummyArgs, _engine: &dyn EngineRef)
    -> Result<serde_json::Value, graph_nexus_core::GnxError>
{
    Ok(serde_json::json!({"ok": true}))
}

gnx_register_mcp_tool!(DummyArgs, run_inner);

#[test]
fn macro_registers_dummy_tool_via_inventory() {
    let found: Vec<&'static graph_nexus_mcp::registry::GnxMcpTool> =
        inventory::iter::<graph_nexus_mcp::registry::GnxMcpTool>().collect();
    let names: Vec<&str> = found.iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"gnx_macro_test"),
        "expected gnx_macro_test in registry; got {:?}", names
    );
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus-mcp --test macro_test
```

Expected: FAIL — `gnx_register_mcp_tool` macro doesn't exist.

- [ ] **Step 3: Define the macro**

```rust
// crates/graph-nexus-mcp/src/macros.rs

/// Register a CLI command as an MCP tool. Called once at the bottom of
/// each `commands/<x>.rs` file that should appear as an MCP tool.
///
/// Tool name is auto-derived from `module_path!()` — adding the file
/// to the module tree is enough.
#[macro_export]
macro_rules! gnx_register_mcp_tool {
    ($args:ty, $inner:path) => {
        inventory::submit! {
            $crate::registry::GnxMcpTool {
                name: $crate::registry::derive_tool_name(module_path!()),
                description: <$args as ::schemars::JsonSchema>::schema_name(),
                schema: || ::schemars::schema_for!($args),
                handler: |raw, engine| {
                    let parsed: $args = ::serde_json::from_value(raw)
                        .map_err(|e| ::graph_nexus_core::GnxError::InvalidArgument(
                            format!("MCP args decode: {e}")))?;
                    $inner(parsed, engine)
                },
                subcommand: $crate::registry::derive_subcommand(module_path!()),
            }
        }
    };
}
```

Edit `crates/graph-nexus-mcp/src/lib.rs`:

```rust
pub mod argv;
pub mod daemon;
pub mod macros;       // ← add
pub mod registry;
pub mod server;
pub mod spawn;
```

- [ ] **Step 4: Run, see pass**

```bash
cargo test -p graph-nexus-mcp --test macro_test
```

Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/macros.rs crates/graph-nexus-mcp/src/lib.rs crates/graph-nexus-mcp/tests/macro_test.rs
git commit -m "feat(mcp): gnx_register_mcp_tool! macro + inventory submit

One-line registration. Tool name auto-derived from module_path!();
description from schemars JsonSchema; handler closes over the command's
run_inner. Compile-test fixture confirms expansion + inventory pickup."
```

---

### Task 12: Add macro line to each of the 8 in-scope commands

**Files:**
- Modify: 8 files — `crates/graph-nexus-cli/src/commands/{context,impact,query,detect_changes,rename,route_map,shape_check,multi_query}.rs`

- [ ] **Step 1: Write the inventory completeness test**

Create `crates/graph-nexus-cli/tests/mcp_tool_registry_test.rs`:

```rust
//! Verifies all 8 in-scope commands self-registered via inventory.
//! New 9th command added later: this test updates with one row.

#[test]
fn expected_eight_tools_present() {
    // Force-link by referencing the modules.
    let _ = std::mem::discriminant(&());
    // The CLI binary itself must register on link; pull it in via
    // graph-nexus-cli library.
    use graph_nexus_mcp::registry::GnxMcpTool;
    let names: std::collections::BTreeSet<&str> =
        inventory::iter::<GnxMcpTool>().map(|t| t.name).collect();
    let expected = [
        "gnx_context",
        "gnx_detect_changes",
        "gnx_impact",
        "gnx_multi_query",
        "gnx_query",
        "gnx_rename",
        "gnx_route_map",
        "gnx_shape_check",
    ];
    for tool in expected {
        assert!(names.contains(tool), "missing {tool} in registry; got {names:?}");
    }
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus --test mcp_tool_registry_test
```

Expected: FAIL — no tools registered yet.

- [ ] **Step 3: Add macro line to each of 8 files**

Append to the very bottom of EACH command file (after the `#[cfg(test)] mod tests`):

```rust
graph_nexus_mcp::gnx_register_mcp_tool!(ContextArgs, run_inner);
```

Replace `ContextArgs` with the file's own Args type:
- `commands/context.rs` → `ContextArgs, run_inner`
- `commands/impact.rs` → `ImpactArgs, run_inner`
- `commands/query.rs` → `QueryArgs, run_inner`
- `commands/detect_changes.rs` → `DetectChangesArgs, run_inner`
- `commands/rename.rs` → `RenameArgs, run_inner`
- `commands/route_map.rs` → `RouteMapArgs, run_inner`
- `commands/shape_check.rs` → `ShapeCheckArgs, run_inner`
- `commands/multi_query.rs` → `MultiQueryArgs, run_inner`

- [ ] **Step 4: Run, see pass**

```bash
cargo test -p graph-nexus --test mcp_tool_registry_test
```

Expected: 1 test passes — all 8 tools discovered.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/*.rs crates/graph-nexus-cli/tests/mcp_tool_registry_test.rs
git commit -m "feat(cli): register 8 commands as MCP tools via inventory macro

One gnx_register_mcp_tool! line per command file. Tool names derived
from module_path!() — zero hardcoding. Test invariant: registry must
report exactly these 8 tool names."
```

---

### Task 13: Spawn-mode dispatch handler

**Files:**
- Modify: `crates/graph-nexus-mcp/src/spawn.rs`
- Create: `crates/graph-nexus-mcp/tests/spawn_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/graph-nexus-mcp/tests/spawn_test.rs
//! Unit test for spawn-mode dispatch — invokes a stub script that
//! echoes its arguments back, then verifies dispatch wrapped it
//! correctly. Avoids depending on a built gnx binary for this layer.

use graph_nexus_mcp::spawn::run_spawn;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_stub(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    let stub = dir.join("gnx");
    std::fs::write(&stub, script).unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    stub
}

#[test]
fn spawn_invokes_subcommand_and_captures_stdout() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(
        dir.path(),
        "#!/bin/sh\necho \"sub=$1 arg1=$2 arg2=$3\"\n",
    );
    let out = run_spawn(&stub, "context", &json!({"name": "foo"})).unwrap();
    assert!(out.contains("sub=context"));
    assert!(out.contains("arg1=--name"));
    assert!(out.contains("arg2=foo"));
}

#[test]
fn spawn_subprocess_failure_returns_err_with_stderr() {
    let dir = TempDir::new().unwrap();
    let stub = write_stub(dir.path(), "#!/bin/sh\necho 'boom' >&2\nexit 1\n");
    let err = run_spawn(&stub, "context", &json!({})).unwrap_err();
    assert!(err.to_string().contains("boom"));
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus-mcp --test spawn_test
```

Expected: FAIL — `run_spawn` not defined.

- [ ] **Step 3: Implement `spawn.rs`**

```rust
// crates/graph-nexus-mcp/src/spawn.rs
//! Spawn-mode dispatch. Each tool call → `Command::new(gnx).arg(subcmd).args(argv).output()`.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Synchronously invoke `<binary> <subcommand> [argv...]` and return
/// captured stdout on success. Non-zero exit → Err containing stderr.
pub fn run_spawn(binary: &Path, subcommand: &str, args: &Value) -> Result<String> {
    let argv = crate::argv::json_to_argv(args)?;
    let output = std::process::Command::new(binary)
        .arg(subcommand)
        .args(&argv)
        .output()
        .with_context(|| format!("spawning {binary:?} {subcommand}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "gnx {subcommand} exited with {} — stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
```

- [ ] **Step 4: Run, see pass**

```bash
cargo test -p graph-nexus-mcp --test spawn_test
```

Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/spawn.rs crates/graph-nexus-mcp/tests/spawn_test.rs
git commit -m "feat(mcp): spawn-mode dispatch — run_spawn(binary, subcommand, args)

Synchronously spawns the gnx binary, captures stdout, surfaces
non-zero-exit stderr as Err. Tests use a shell stub for isolation."
```

---

### Task 14: Daemon-mode dispatch with mtime-remap

**Files:**
- Modify: `crates/graph-nexus-mcp/src/daemon.rs`
- Create: `crates/graph-nexus-mcp/tests/daemon_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/graph-nexus-mcp/tests/daemon_test.rs
use graph_nexus_mcp::daemon::needs_remap;
use std::fs;
use std::time::SystemTime;
use tempfile::TempDir;

#[test]
fn needs_remap_false_when_mtime_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.bin");
    fs::write(&path, b"v1").unwrap();
    let loaded_at = fs::metadata(&path).unwrap().modified().unwrap();
    assert!(!needs_remap(&path, loaded_at).unwrap());
}

#[test]
fn needs_remap_true_when_file_atomically_replaced() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.bin");
    fs::write(&path, b"v1").unwrap();
    let loaded_at = fs::metadata(&path).unwrap().modified().unwrap();

    // Atomic replace via rename — mtime of the path's new inode is later.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let tmp = dir.path().join("graph.bin.tmp");
    fs::write(&tmp, b"v2").unwrap();
    fs::rename(&tmp, &path).unwrap();

    assert!(needs_remap(&path, loaded_at).unwrap());
}

#[test]
fn needs_remap_errors_if_path_missing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("never_existed.bin");
    let res = needs_remap(&path, SystemTime::UNIX_EPOCH);
    assert!(res.is_err());
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus-mcp --test daemon_test
```

Expected: FAIL — `needs_remap` not defined.

- [ ] **Step 3: Implement daemon.rs**

```rust
// crates/graph-nexus-mcp/src/daemon.rs
//! Daemon-mode dispatch. Engine mmap'd once at server startup; refreshed
//! via mtime-remap before every dispatch.
//!
//! Why mtime-remap: `gnx analyze` writes graph.bin via atomic
//! write-tmp + rename (see crates/graph-nexus-core/src/registry/io.rs:33).
//! This swaps the dentry but our existing mmap holds the unlinked old
//! inode. Without explicit re-load, daemon serves stale data forever.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::SystemTime;

/// True iff the file at `path` has been replaced since `loaded_at`.
/// Returns Err if the file is missing or unreadable (caller decides
/// whether to abort or retry).
pub fn needs_remap(path: &Path, loaded_at: SystemTime) -> Result<bool> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat {path:?} for mtime-remap check"))?;
    let mtime = meta
        .modified()
        .with_context(|| format!("modified() for {path:?}"))?;
    Ok(mtime > loaded_at)
}
```

(Daemon-mode dispatch itself — the function that loads Engine, looks up
the tool's handler, calls it, wraps the Value through `emit_to_string` —
is wired up in Task 15 alongside the server scaffold, because Engine
lifecycle is hostly tied to the server's main loop.)

- [ ] **Step 4: Run, see pass**

```bash
cargo test -p graph-nexus-mcp --test daemon_test
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/daemon.rs crates/graph-nexus-mcp/tests/daemon_test.rs
git commit -m "feat(mcp): daemon-mode mtime-remap detection

needs_remap(path, loaded_at) returns true iff the file's current
mtime is newer than the recorded load time — i.e. someone ran
gnx analyze and our mmap is now pointing at the unlinked old inode."
```

---

### Task 15: MCP server stdio loop

**Files:**
- Modify: `crates/graph-nexus-mcp/src/server.rs`
- Create: `crates/graph-nexus-mcp/tests/server_smoke_test.rs`

- [ ] **Step 1: Write the smoke test**

```rust
// crates/graph-nexus-mcp/tests/server_smoke_test.rs
//! Smoke: build a server, list-tools, expect the inventory contents.

use graph_nexus_mcp::server::{DispatchMode, GnxMcpServer};

#[tokio::test(flavor = "current_thread")]
async fn list_tools_returns_registered_inventory() {
    let server = GnxMcpServer::new(DispatchMode::Spawn).expect("init");
    let tools = server.list_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    // From this test crate's perspective, the CLI commands aren't
    // linked in; only fixtures/macros submitted by graph-nexus-mcp's
    // own tests register. The smoke test only asserts the API works
    // — empty registry is acceptable here. End-to-end with CLI tools
    // is exercised by the integration test in Task 17.
    let _ = names;  // shape compile-check only
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus-mcp --test server_smoke_test
```

Expected: FAIL — `GnxMcpServer` not defined.

- [ ] **Step 3: Implement server.rs**

```rust
// crates/graph-nexus-mcp/src/server.rs
//! Stdio JSON-RPC MCP server scaffold. Wraps `rmcp::ServiceExt` and
//! dispatches tool calls via either spawn or daemon mode.

use crate::registry::{EngineRef, GnxMcpTool};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub enum DispatchMode {
    /// Default: spawn `gnx <subcommand>` per call.
    Spawn,
    /// Opt-in: keep Engine mmap'd; mtime-remap before each call.
    Daemon,
}

/// Owned engine wrapper for daemon mode. Held inside an Arc<Mutex<>>
/// by the server when daemon mode is active so concurrent tool calls
/// serialize at the engine boundary (mmap is shareable but our handler
/// fn signature takes &dyn EngineRef which we keep simple).
pub struct DaemonState {
    pub engine_path: PathBuf,
    pub loaded_at: std::time::SystemTime,
    // Real Engine handle would be here in full impl; abstracted as
    // EngineRef so this crate doesn't depend on graph-nexus-cli's
    // private Engine type. For wiring see commands/mcp.rs Task 16.
}

impl EngineRef for DaemonState {
    fn graph_path(&self) -> &std::path::Path {
        &self.engine_path
    }
}

pub struct GnxMcpServer {
    mode: DispatchMode,
    daemon_state: Option<Arc<std::sync::Mutex<DaemonState>>>,
    /// Path to the current gnx binary (used by spawn mode).
    self_exe: PathBuf,
}

impl GnxMcpServer {
    pub fn new(mode: DispatchMode) -> Result<Self> {
        let self_exe = std::env::current_exe()
            .context("locating current_exe for spawn dispatch")?;
        Ok(Self { mode, daemon_state: None, self_exe })
    }

    pub fn with_daemon_state(mut self, state: DaemonState) -> Self {
        self.daemon_state = Some(Arc::new(std::sync::Mutex::new(state)));
        self
    }

    /// Enumerate all tools registered via inventory at link time.
    pub fn list_tools(&self) -> Vec<&'static GnxMcpTool> {
        inventory::iter::<GnxMcpTool>().collect()
    }

    /// Dispatch a single tool call. The server's stdio loop calls this
    /// for each `tools/call` JSON-RPC frame.
    pub async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<String> {
        let tool = self
            .list_tools()
            .into_iter()
            .find(|t| t.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
        match self.mode {
            DispatchMode::Spawn => {
                crate::spawn::run_spawn(&self.self_exe, tool.subcommand, &args)
            }
            DispatchMode::Daemon => {
                let state_arc = self
                    .daemon_state
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("daemon mode requires DaemonState"))?;
                let mut state = state_arc.lock().unwrap();
                // mtime-remap probe (cheap stat)
                if crate::daemon::needs_remap(&state.engine_path, state.loaded_at)? {
                    state.loaded_at = std::fs::metadata(&state.engine_path)?.modified()?;
                    // Real Engine reload happens here in the wiring task;
                    // see crates/graph-nexus-cli/src/commands/mcp.rs.
                }
                let value = (tool.handler)(args, &*state)
                    .map_err(|e| anyhow::anyhow!("tool handler: {e}"))?;
                Ok(serde_json::to_string(&value)?)
            }
        }
    }
}
```

(The actual `rmcp::ServiceExt::serve_stdio()` wiring lives in
`commands/mcp.rs` from Task 16 since it needs CLI-side Engine
instantiation.)

- [ ] **Step 4: Run, see pass**

```bash
cargo test -p graph-nexus-mcp --test server_smoke_test
```

Expected: smoke test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-mcp/src/server.rs crates/graph-nexus-mcp/tests/server_smoke_test.rs
git commit -m "feat(mcp): GnxMcpServer scaffold with dual-mode dispatch

Spawn mode shells out via run_spawn; daemon mode runs the inventory
handler in-process after mtime-remap check. list_tools enumerates the
inventory. rmcp serve_stdio wiring lives in commands/mcp.rs."
```

---

### Task 16: `gnx mcp` subcommand (CLI side)

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/mcp.rs`
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs`
- Modify: `crates/graph-nexus-cli/src/main.rs`

- [ ] **Step 1: Write the integration test (drives the binary)**

```rust
// crates/graph-nexus-cli/tests/mcp_subcommand_test.rs
//! Drives `gnx mcp tools` and asserts the output enumerates the 8
//! expected tools.

use std::process::Command;

#[test]
fn gnx_mcp_tools_lists_eight_tools() {
    let bin = env!("CARGO_BIN_EXE_gnx");
    let out = Command::new(bin).args(["mcp", "tools"]).output().expect("spawn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    for expected in [
        "gnx_context", "gnx_impact", "gnx_query", "gnx_detect_changes",
        "gnx_rename", "gnx_route_map", "gnx_shape_check", "gnx_multi_query",
    ] {
        assert!(stdout.contains(expected), "missing {expected} in:\n{stdout}");
    }
}
```

- [ ] **Step 2: Run, see fail**

```bash
cargo test -p graph-nexus --test mcp_subcommand_test
```

Expected: FAIL — no `mcp` subcommand exists yet.

- [ ] **Step 3: Implement `commands/mcp.rs`**

```rust
// crates/graph-nexus-cli/src/commands/mcp.rs
//! `gnx mcp` subcommand: serve | tools.

use clap::{Args, Subcommand};
use graph_nexus_core::GnxError;
use graph_nexus_mcp::server::{DispatchMode, GnxMcpServer};

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Run stdio JSON-RPC MCP server.
    Serve {
        /// Use daemon mode (keep Engine mmap'd; mtime-remap before
        /// each call). Default is spawn mode.
        #[arg(long, default_value_t = false)]
        daemon: bool,
        /// Optional path to graph.bin (daemon mode only; defaults to
        /// the same resolution gnx commands use).
        #[arg(long)]
        graph: Option<String>,
    },
    /// List tools that would be exposed by `serve`. Useful for debug
    /// and for the test invariant.
    Tools,
}

pub fn run(args: McpArgs) -> Result<(), GnxError> {
    match args.action {
        McpAction::Tools => {
            let server = GnxMcpServer::new(DispatchMode::Spawn)
                .map_err(|e| GnxError::Output(format!("server init: {e}")))?;
            for tool in server.list_tools() {
                println!("{}\t{}", tool.name, tool.description);
            }
            Ok(())
        }
        McpAction::Serve { daemon, graph: _ } => {
            let mode = if daemon { DispatchMode::Daemon } else { DispatchMode::Spawn };
            let mut server = GnxMcpServer::new(mode)
                .map_err(|e| GnxError::Output(format!("server init: {e}")))?;
            if daemon {
                // Daemon mode needs an Engine + path. Defer the full
                // wiring (Engine::load + register Mutex inside server)
                // to follow-up — for now require explicit --graph flag
                // OR fall back to a stub path. Production wiring lands
                // alongside the TUI install handler in subproject C.
                let _ = &mut server;
                return Err(GnxError::InvalidArgument(
                    "daemon mode wiring lands with subproject C; \
                     use spawn mode (omit --daemon) for now".into()
                ));
            }
            // Spawn-mode stdio loop via rmcp.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| GnxError::Output(format!("tokio runtime: {e}")))?;
            rt.block_on(async move {
                graph_nexus_mcp::server::serve_stdio(server).await
                    .map_err(|e| GnxError::Output(format!("serve_stdio: {e}")))
            })?;
            Ok(())
        }
    }
}
```

Also add to `crates/graph-nexus-mcp/src/server.rs` (append to the existing module):

```rust
/// rmcp stdio transport loop. Reads JSON-RPC frames from stdin, writes
/// responses to stdout. Blocks until the host disconnects.
pub async fn serve_stdio(server: GnxMcpServer) -> anyhow::Result<()> {
    use rmcp::{ServerHandler, transport::stdio};
    // The rmcp 0.2 API takes a service implementing ServerHandler.
    // Bind our GnxMcpServer to that trait inline.
    impl rmcp::ServerHandler for GnxMcpServer {
        // rmcp's actual method names — verify against installed
        // version. Common shape (as of rmcp 0.2):
        //   async fn list_tools(...) -> ListToolsResult
        //   async fn call_tool(name, args, ...) -> CallToolResult
        // The wiring is mechanical: enumerate self.list_tools(),
        // turn each GnxMcpTool into rmcp::Tool { name, description,
        // input_schema: (schema)() }; for call_tool delegate to
        // self.call_tool(name, args).
    }
    let transport = stdio();
    server.serve(transport).await?;
    Ok(())
}
```

NOTE: the `serve_stdio` implementation above is **shape-illustrative**.
The exact `rmcp` API signatures depend on the installed version — at
implementation time, run `cargo doc -p rmcp --open` to confirm method
names and adjust the trait impl accordingly. The pattern (turn each
`GnxMcpTool` into an rmcp tool registration; delegate `call_tool` to
`GnxMcpServer::call_tool`) is the only thing that's fixed.

- [ ] **Step 4: Wire into mod.rs + main.rs**

Edit `crates/graph-nexus-cli/src/commands/mod.rs`, add:

```rust
pub mod mcp;
```

Edit `crates/graph-nexus-cli/src/main.rs`, add to the `Commands` enum:

```rust
/// MCP server / tool inspection
Mcp(commands::mcp::McpArgs),
```

Add to the no-graph-needed early-return section:

```rust
if let Commands::Mcp(args) = &cli.command {
    if let Err(e) = commands::mcp::run(args.clone()) {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
    return;
}
```

Add to the `repo_opt` extraction match arm (None — mcp doesn't take repo):

```rust
| Commands::Mcp(_)
```

Add to the final dispatch fallthrough (Ok(()) — handled above):

```rust
| Commands::Mcp(_)
```

- [ ] **Step 5: Run + commit**

```bash
cargo build -p graph-nexus
cargo test -p graph-nexus --test mcp_subcommand_test
```

Expected: build clean; tools-listing test passes.

```bash
git add \
  crates/graph-nexus-cli/src/commands/mcp.rs \
  crates/graph-nexus-cli/src/commands/mod.rs \
  crates/graph-nexus-cli/src/main.rs \
  crates/graph-nexus-mcp/src/server.rs \
  crates/graph-nexus-cli/tests/mcp_subcommand_test.rs
git commit -m "feat(cli): gnx mcp serve / tools subcommand

gnx mcp tools enumerates the 8 in-scope registered tools (test
invariant). gnx mcp serve runs the stdio JSON-RPC server in spawn
mode; --daemon flag returns a friendly error pending subproject C
wiring (which adds the Engine handle daemon mode needs)."
```

---

## Phase 4 — End-to-end + docs

### Task 17: End-to-end test — manual MCP client against `gnx mcp serve`

**Files:**
- Create: `crates/graph-nexus-mcp/tests/server_e2e.rs`

- [ ] **Step 1: Write the e2e test**

```rust
// crates/graph-nexus-mcp/tests/server_e2e.rs
//! End-to-end: pipe a real MCP JSON-RPC sequence into `gnx mcp serve`'s
//! stdin, read the JSON-RPC response, assert it lists the 8 tools and
//! a tools/call against gnx_context returns plausible output.

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> String {
    env!("CARGO_BIN_EXE_gnx").to_string()
}

#[test]
fn mcp_server_lists_eight_tools_via_json_rpc() {
    let mut child = Command::new(gnx_bin())
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gnx mcp serve");
    {
        let stdin = child.stdin.as_mut().unwrap();
        // MCP initialize handshake then tools/list.
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        writeln!(stdin, "{init}").unwrap();
        writeln!(stdin, "{list}").unwrap();
    }
    // Drop stdin to close the server's input side.
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for tool in [
        "gnx_context", "gnx_impact", "gnx_query", "gnx_detect_changes",
        "gnx_rename", "gnx_route_map", "gnx_shape_check", "gnx_multi_query",
    ] {
        assert!(stdout.contains(tool), "missing {tool} in MCP tools/list response:\n{stdout}");
    }
}
```

- [ ] **Step 2: Run it**

```bash
cargo test -p graph-nexus-mcp --test server_e2e
```

Expected: PASS — `gnx mcp serve` honors MCP protocol and lists 8 tools.

If FAIL: most likely the `rmcp::ServerHandler` impl in Task 16 needs
adjustment to match the installed rmcp version. Run `cargo doc -p rmcp`
locally, align the method names, retry.

- [ ] **Step 3: Commit**

```bash
git add crates/graph-nexus-mcp/tests/server_e2e.rs
git commit -m "test(mcp): e2e — pipe JSON-RPC into gnx mcp serve

Spawns the real gnx binary, sends initialize + tools/list, asserts
the 8 expected tools appear in the response. Validates the full
stdio → rmcp → inventory pipeline end to end."
```

---

### Task 18: README + spec status update

**Files:**
- Modify: `README.md`
- Modify: `docs/specs/2026-05-15-host-integration.md`

- [ ] **Step 1: Add MCP usage section to README**

After the existing "## ⚡ Usage" section in `README.md`, add:

```markdown
### MCP server (for LLM hosts)

`gnx` ships an MCP server exposing the 8 core commands as MCP tools.
Hosts that speak MCP (Claude Code, Cursor, Windsurf, Cline, Codex CLI,
Gemini CLI, etc.) can register `gnx` and call the tools autonomously.

```bash
# Inspect what tools will be exposed
gnx mcp tools

# Run the MCP server (default: spawn mode — fresh subprocess per call)
gnx mcp serve

# Or daemon mode — Engine mmap'd, mtime-remap on graph rebuild
# (full wiring lands with `gnx admin` TUI; for now spawn mode only)
gnx mcp serve --daemon    # currently returns NotImplementedError, pending TUI install handler
```

Manual host config example for Claude Code (`~/.config/claude-code/mcp-servers.json`):

```json
{
  "mcpServers": {
    "gnx": { "command": "gnx", "args": ["mcp", "serve"] }
  }
}
```

A `gnx admin` TUI for one-command installation across multiple hosts
ships in a follow-up release.
```

- [ ] **Step 2: Update spec status**

Edit `docs/specs/2026-05-15-host-integration.md` near the top:

```markdown
**Status**: Spec landed; subproject A (graph-nexus-mcp crate, single-binary
`gnx mcp serve`, spawn-mode dispatch) implemented and shipped. Daemon-mode
wiring deferred to subproject C (TUI install handler — needs Engine handle
from registry resolution). Subprojects B/D/E still pending.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/specs/2026-05-15-host-integration.md
git commit -m "docs: README MCP section + spec status

Documents gnx mcp serve / tools subcommands and manual host config
for Claude Code. Marks subproject A as shipped in the host-integration
spec; B/C/D/E still pending."
```

---

## Spec coverage check

Going back through `docs/specs/2026-05-15-host-integration.md` section by section:

| Spec section | Implementing task |
|---|---|
| Single-binary `gnx mcp serve` | Task 16 |
| `gnx mcp serve --daemon` flag | Task 16 (parses); Task 14 (mtime-remap); daemon-dispatch wiring deferred to subproject C |
| Shared business-logic refactor (`run_inner`) | Tasks 5–10 |
| Output handling refactor (`emit_to_string`) | Task 1 |
| MCP crate infrastructure (registry + inventory) | Task 3 |
| `gnx_register_mcp_tool!` macro | Task 11 |
| Per-command self-registration | Task 12 |
| Tool set (auto-discovered 8) | Task 12 (registration); Task 16 (`gnx mcp tools` enumerates) |
| Spawn-mode dispatch | Task 13 |
| `json_to_argv` | Task 4 |
| Daemon-mode mtime-remap | Task 14 |
| Inventory completeness invariant | Task 12 (test) |
| Three-state idempotent upsert install | **NOT in this plan** — that's subproject C (TUI install handler) |
| Codex / Gemini fork patches | **NOT in this plan** — subprojects D/E |

Coverage of subproject A: complete. Items deferred are explicitly labelled as belonging to subprojects B/C/D/E.

## Final verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

All must pass before this plan is considered complete.
