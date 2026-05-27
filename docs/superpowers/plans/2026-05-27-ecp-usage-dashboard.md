# `ecp usage` Usage Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ecp usage` — a terminal ASCII usage dashboard (invocation counts, latency vs the <30 ms budget, persisted error log) — plus the CLI-path telemetry instrumentation that feeds it.

**Architecture:** Three layers. (1) **Collection**: `main()` is refactored so all dispatch flows through one `Result`-returning inner function; `main()` wraps it in a timer and appends one `CallRecord` per invocation (success or failure) to `~/.ecp/telemetry/<repo>/cli-calls.jsonl`, best-effort, no flock, no sync — zero hot-path cost. (2) **Aggregation**: a new `usage` reader merges the CLI file + the existing MCP `calls.jsonl`, reusing the percentile/grouping shape from `insight.rs`. (3) **Presentation**: an ASCII dashboard with auto color detection. The shared `CallRecord` struct moves from `ecp-mcp` to `ecp-core` and gains two append-only fields (`source`, `error_kind`); the MCP path keeps working unchanged.

**Tech Stack:** Rust, clap (derive), serde / serde_json, `std::io::IsTerminal` (zero-dep color detection), existing `ecp_core::time` RFC3339 helpers.

**Spec:** `docs/superpowers/specs/2026-05-27-ecp-usage-usage-dashboard-design.md`

---

## File Structure

| File | Responsibility | Action |
|------|----------------|--------|
| `crates/ecp-core/src/telemetry.rs` | Shared `CallRecord` (+`source`,`error_kind`) + `append_record(dir, filename, &record)`; the single struct both CLI and MCP serialize. | **Create** (lift from ecp-mcp) |
| `crates/ecp-core/src/lib.rs` | `pub mod telemetry;` | Modify |
| `crates/ecp-mcp/src/telemetry.rs` | Keep the cached-BufWriter MCP writer; re-export `CallRecord` from `ecp_core` instead of defining it. | Modify |
| `crates/ecp-mcp/src/server.rs` | `CallRecord` literal gains `source:"mcp", error_kind:None`. | Modify |
| `crates/ecp-cli/src/telemetry_cli.rs` | CLI-side recorder: classify error → `error_kind`, build `CallRecord{source:"cli"}`, append to `cli-calls.jsonl`; opt-out check; arg-summary helper. | **Create** |
| `crates/ecp-cli/src/main.rs` | Refactor dispatch into `fn dispatch(cli) -> Result<&'static str, EcpError>` (returns subcommand label) + timer wrapper in `main()`. | Modify |
| `crates/ecp-cli/src/commands/usage.rs` | `UsageArgs`, `run`, aggregation (reuses insight shapes), ASCII rendering, color detection, retention prune. | **Create** |
| `crates/ecp-cli/src/commands/mod.rs` | `pub mod usage;` | Modify |
| `crates/ecp-cli/src/cli.rs` | `Gain(commands::usage::UsageArgs)` variant in `Commands`. | Modify |
| `crates/ecp-cli/src/config_parser.rs` | Parse optional `[telemetry]` section: `cli` (bool, default true), `retention_days` (u64, default 7). | Modify |
| `crates/ecp-cli/tests/usage_cmd.rs` | Integration tests: collection, aggregation, rendering, color, retention, taxonomy. | **Create** |

---

## Task 1: Lift `CallRecord` into `ecp-core` with the two new fields

**Files:**
- Create: `crates/ecp-core/src/telemetry.rs`
- Modify: `crates/ecp-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/ecp-core/src/telemetry.rs` (create the file with this test at the bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_record_serializes_all_fields() {
        let r = CallRecord {
            ts: "2026-05-27T07:00:00Z",
            tool: "inspect",
            duration_ms: 6,
            ok: true,
            source: "cli",
            error_kind: None,
        };
        let line = serde_json::to_string(&r).unwrap();
        assert!(line.contains(r#""source":"cli""#));
        assert!(line.contains(r#""tool":"inspect""#));
        // error_kind None must still serialize (null) so readers can rely on the key
        assert!(line.contains(r#""error_kind":null"#));
    }

    #[test]
    fn append_record_writes_one_line() {
        let dir = std::env::temp_dir().join(format!("ecp-tlm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = CallRecord {
            ts: "2026-05-27T07:00:00Z",
            tool: "find",
            duration_ms: 4,
            ok: false,
            source: "cli",
            error_kind: Some("no-such-symbol"),
        };
        append_record(&dir, "cli-calls.jsonl", &r);
        let body = std::fs::read_to_string(dir.join("cli-calls.jsonl")).unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains(r#""error_kind":"no-such-symbol""#));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ecp-core telemetry`
Expected: FAIL — `CallRecord` / `append_record` not defined.

- [ ] **Step 3: Write minimal implementation**

Put this ABOVE the `#[cfg(test)]` block in `crates/ecp-core/src/telemetry.rs`:

```rust
//! Shared telemetry record + best-effort jsonl appender.
//!
//! One [`CallRecord`] is appended per invocation — by the CLI (one process
//! per command, file `cli-calls.jsonl`) and by the MCP server (long-lived,
//! file `calls.jsonl`, via its own cached-writer wrapper in ecp-mcp).
//!
//! Schema is **unstable (v1)**. New fields are append-only and optional on
//! read (`#[serde(default)]`) so existing files stay parseable.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

/// One record appended per invocation. CLI and MCP share this exact struct.
#[derive(serde::Serialize)]
pub struct CallRecord<'a> {
    /// RFC3339 UTC timestamp of the call start.
    pub ts: &'a str,
    /// Subcommand (CLI: `"inspect"`) or MCP tool name (`"ecp_inspect"`).
    pub tool: &'a str,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// `true` on success, `false` on error.
    pub ok: bool,
    /// `"cli"` or `"mcp"`. Distinguishes the two invocation paths.
    pub source: &'a str,
    /// Failure class (e.g. `"no-such-symbol"`); `None` when `ok == true`.
    pub error_kind: Option<&'a str>,
}

/// Append one jsonl line to `dir/filename`. Best-effort: all I/O errors are
/// silently dropped — telemetry MUST NOT affect the caller's result. Single
/// `O_APPEND` write of a sub-PIPE_BUF line is atomic under POSIX, so no lock.
pub fn append_record(dir: &Path, filename: &str, record: &CallRecord<'_>) {
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(filename))
    {
        let _ = writeln!(f, "{line}");
    }
}
```

- [ ] **Step 4: Register the module.** In `crates/ecp-core/src/lib.rs`, add `pub mod telemetry;` next to the other `pub mod` lines (find one with `grep -n "pub mod time;" crates/ecp-core/src/lib.rs` and add adjacent).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p ecp-core telemetry`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-core/src/telemetry.rs crates/ecp-core/src/lib.rs
git commit -m "feat(core): shared CallRecord + append_record with source/error_kind"
```

---

## Task 2: Point the MCP writer at the shared struct

**Files:**
- Modify: `crates/ecp-mcp/src/telemetry.rs`
- Modify: `crates/ecp-mcp/src/server.rs`

**Context:** ecp-mcp keeps its cached-`BufWriter` writer (long-lived process — worth the cache). Only the struct definition moves; the writer now serializes `ecp_core::telemetry::CallRecord`.

- [ ] **Step 1: Re-export instead of defining.** In `crates/ecp-mcp/src/telemetry.rs`, delete the local `pub struct CallRecord { ... }` (the `#[derive(serde::Serialize)] pub struct CallRecord<'a> { ts, tool, duration_ms, ok }` block) and replace with:

```rust
pub use ecp_core::telemetry::CallRecord;
```

Keep everything else (`REPO_KEY`, `WRITER`, `get_writer`, `append`, `append_to`, `append_inner`, `init_repo_id`, `rfc3339_now` re-export) unchanged.

- [ ] **Step 2: Fix the MCP CallRecord literal.** In `crates/ecp-mcp/src/server.rs`, the `CallRecord { ts, tool, duration_ms, ok }` literal (in `call_tool`, around the `crate::telemetry::append(&record)` call) gains the two fields:

```rust
        let record = crate::telemetry::CallRecord {
            ts: &ts,
            tool: name,
            duration_ms,
            ok,
            source: "mcp",
            error_kind: None,
        };
```

- [ ] **Step 3: Build to verify both crates compile**

Run: `cargo build -p ecp-mcp -p ecp-core`
Expected: success, no errors.

- [ ] **Step 4: Run the MCP telemetry tests** (if any reference the struct)

Run: `cargo test -p ecp-mcp telemetry`
Expected: PASS (or "0 tests" if none — acceptable).

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-mcp/src/telemetry.rs crates/ecp-mcp/src/server.rs
git commit -m "refactor(mcp): use shared ecp_core CallRecord; source=mcp"
```

---

## Task 3: `[telemetry]` config — `cli` + `retention_days`

**Files:**
- Modify: `crates/ecp-cli/src/config_parser.rs`

**Context:** No `[telemetry]` section exists today. First locate how an existing optional section is parsed: `grep -n "pub struct .*Config\|fn parse\|\[.*\]" crates/ecp-cli/src/config_parser.rs | head -40` and mirror the nearest section's pattern (struct + default + parse). Defaults: `cli = true`, `retention_days = 7`.

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `crates/ecp-cli/src/config_parser.rs` (find `#[cfg(test)] mod tests` or create one):

```rust
    #[test]
    fn telemetry_defaults_when_section_absent() {
        let cfg = parse_config_str("").unwrap();
        assert!(cfg.telemetry.cli);
        assert_eq!(cfg.telemetry.retention_days, 7);
    }

    #[test]
    fn telemetry_section_overrides() {
        let cfg = parse_config_str("[telemetry]\ncli = false\nretention_days = 14\n").unwrap();
        assert!(!cfg.telemetry.cli);
        assert_eq!(cfg.telemetry.retention_days, 14);
    }
```

> NOTE: replace `parse_config_str` with the actual entrypoint the file uses for parsing a TOML string (check the existing test helpers; if tests parse via a `Config::from_str` or a `parse(...)` fn, use that name). If the config is not TOML-based, mirror whatever format the file already parses.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --lib config_parser::tests::telemetry`
Expected: FAIL — no `telemetry` field on the config struct.

- [ ] **Step 3: Implement.** Add a `TelemetryConfig` struct with `serde` defaults and wire it into the top-level config struct:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    /// Record CLI invocations to cli-calls.jsonl. Opt-out via `cli = false`.
    pub cli: bool,
    /// Days of CLI telemetry to keep; older lines pruned off the hot path.
    pub retention_days: u64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { cli: true, retention_days: 7 }
    }
}
```

Add `pub telemetry: TelemetryConfig,` to the top-level config struct, with `#[serde(default)]` on the field (or on the struct) so an absent `[telemetry]` section yields the default.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p egent-code-plexus --lib config_parser::tests::telemetry`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/config_parser.rs
git commit -m "feat(config): [telemetry] cli + retention_days (defaults true/7)"
```

---

## Task 4: CLI recorder — error taxonomy + arg summary + opt-out

**Files:**
- Create: `crates/ecp-cli/src/telemetry_cli.rs`
- Modify: `crates/ecp-cli/src/lib.rs` (add `pub mod telemetry_cli;`)

**Context:** This module is pure logic (no dispatch wiring yet — that's Task 5). It exposes: `classify_error(&EcpError) -> &'static str`, `arg_summary(&Cli) -> String` (short redacted args for `--failures`), `is_enabled() -> bool` (env + config opt-out), and `record(cli, label, duration_ms, result)` which builds the `CallRecord` and calls `ecp_core::telemetry::append_record`.

- [ ] **Step 1: Write the failing test**

Create `crates/ecp-cli/src/telemetry_cli.rs` with this test block at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::EcpError;

    #[test]
    fn classify_maps_known_errors() {
        assert_eq!(classify_error(&EcpError::InvalidArgument("symbol 'foo' not found".into())), "no-such-symbol");
        assert_eq!(classify_error(&EcpError::InvalidArgument("cypher parse error near X".into())), "cypher-parse");
        assert_eq!(classify_error(&EcpError::InvalidArgument("totally novel boom".into())), "other");
    }

    #[test]
    fn opt_out_via_env() {
        std::env::set_var("ECP_NO_TELEMETRY", "1");
        assert!(!is_enabled());
        std::env::remove_var("ECP_NO_TELEMETRY");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --lib telemetry_cli`
Expected: FAIL — module/functions undefined.

- [ ] **Step 3: Implement.** Put above the test block:

```rust
//! CLI-side telemetry recorder. One CallRecord per `ecp <cmd>` invocation
//! (success or failure) is appended to `~/.ecp/telemetry/<repo>/cli-calls.jsonl`.
//! Best-effort; never affects the command's own exit code (see main.rs).

use crate::cli::Cli;
use ecp_core::telemetry::{append_record, CallRecord};
use ecp_core::time::rfc3339_now;
use ecp_core::EcpError;
use std::path::PathBuf;

pub const CLI_TELEMETRY_FILE: &str = "cli-calls.jsonl";

/// Map an error to a small closed taxonomy. Substring match on the message —
/// the error type lacks structured variants for these. Unknown → "other".
pub fn classify_error(e: &EcpError) -> &'static str {
    let msg = e.to_string().to_ascii_lowercase();
    if msg.contains("cypher") && (msg.contains("parse") || msg.contains("label") || msg.contains("syntax")) {
        "cypher-parse"
    } else if msg.contains("not found") || msg.contains("no symbol") || msg.contains("no such symbol") {
        "no-such-symbol"
    } else if msg.contains("stale") || msg.contains("older than head") {
        "index-stale"
    } else if msg.contains("load") && msg.contains("graph") || msg.contains("registry lock") || msg.contains("corrupt") {
        "graph-load-failed"
    } else {
        "other"
    }
}

/// Disabled by `ECP_NO_TELEMETRY` (any value) or config `telemetry.cli=false`.
pub fn is_enabled() -> bool {
    if std::env::var_os("ECP_NO_TELEMETRY").is_some() {
        return false;
    }
    crate::config_parser::load_effective_config()
        .map(|c| c.telemetry.cli)
        .unwrap_or(true)
}

/// Resolve `~/.ecp/telemetry/<repo_key>/` for the current dir. None on failure.
fn telemetry_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let key = crate::repo_identity::repo_dir_name_for_cwd(&cwd).ok()?;
    Some(ecp_core::registry::resolve_home_ecp().join("telemetry").join(key))
}

/// Build + append the record. `label` is the subcommand name; `err` carries the
/// classified kind on failure. No-op when disabled or repo key unresolved.
pub fn record(label: &str, duration_ms: u64, err: Option<&EcpError>) {
    if !is_enabled() {
        return;
    }
    let Some(dir) = telemetry_dir() else { return };
    let kind = err.map(classify_error);
    let ts = rfc3339_now();
    let rec = CallRecord {
        ts: &ts,
        tool: label,
        duration_ms,
        ok: err.is_none(),
        source: "cli",
        error_kind: kind,
    };
    append_record(&dir, CLI_TELEMETRY_FILE, &rec);
}

/// Short, redacted one-line summary of the invoked command for `--failures`.
/// Joins the raw argv (skipping arg0) capped at 80 chars.
pub fn arg_summary(_cli: &Cli) -> String {
    std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}
```

> NOTE on `load_effective_config`: use whatever the codebase exposes for "load the resolved config for cwd". If the real name differs, `grep -n "pub fn .*config\|fn load.*[Cc]onfig" crates/ecp-cli/src/config_parser.rs` and call that; if loading is fallible, keep the `.unwrap_or(true)` fail-open. `arg_summary` takes `_cli` for a stable signature even though it reads `env::args` — keep the param so a future structured summary needs no signature change.

- [ ] **Step 4: Register module.** In `crates/ecp-cli/src/lib.rs` add `pub mod telemetry_cli;`.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p egent-code-plexus --lib telemetry_cli`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/telemetry_cli.rs crates/ecp-cli/src/lib.rs
git commit -m "feat(cli): telemetry recorder — taxonomy, opt-out, arg summary"
```

---

## Task 5: Refactor `main()` dispatch to a single recorded exit point

**Files:**
- Modify: `crates/ecp-cli/src/main.rs`

**Context:** Today `main()` has several `eprintln!("Command failed: {e}"); std::process::exit(1)` sites (the `run_no_graph!` macro, the graph-load failure arms, the final match). To record EVERY invocation once, the body becomes a `Result`-returning inner fn that also yields the subcommand label; `main()` times it, records, prints the error if any, and exits. This is the cleanest single-instrumentation-point approach and removes duplicated exit handling.

- [ ] **Step 1: Write the failing test** (integration — proves a real invocation writes a line)

Create `crates/ecp-cli/tests/usage_cmd.rs` with:

```rust
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn invocation_appends_one_cli_telemetry_line() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // A guaranteed-fast no-graph command: `find` against an empty dir errors
    // out, but it MUST still record one line with ok=false.
    let out = Command::new(ecp_bin())
        .args(["find", "definitely_no_such_symbol_xyz"])
        .current_dir(&tmp)
        .env("HOME", &tmp) // redirect ~/.ecp into tmp
        .env_remove("ECP_NO_TELEMETRY")
        .output()
        .unwrap();
    // The command itself may fail (no graph) — we only assert telemetry wrote.
    let _ = out;
    // Find the cli-calls.jsonl somewhere under tmp/.ecp/telemetry/*/
    let tel_root = tmp.join(".ecp/telemetry");
    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(&tel_root) {
        for e in entries.flatten() {
            let f = e.path().join("cli-calls.jsonl");
            if f.exists() {
                let body = std::fs::read_to_string(&f).unwrap();
                assert!(body.lines().count() >= 1, "expected >=1 telemetry line");
                assert!(body.contains(r#""source":"cli""#));
                found = true;
            }
        }
    }
    assert!(found, "no cli-calls.jsonl written under {tel_root:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}
```

> NOTE: confirm `resolve_home_ecp()` honors `HOME` (it should — `grep -n "resolve_home_ecp" crates/ecp-core/src/registry/*.rs` and check it reads `$HOME`/dirs). If it instead uses a non-overridable path, set the env var it actually reads, or use the `--graph`/test hooks the other integration tests use (see `crates/ecp-cli/tests/insight_cmd.rs` for the established pattern).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test usage_cmd invocation_appends`
Expected: FAIL — no cli-calls.jsonl written (instrumentation not wired).

- [ ] **Step 3: Implement the refactor.** Restructure `main()` in `crates/ecp-cli/src/main.rs`:

1. Add a label helper:

```rust
/// Stable subcommand label for telemetry (matches the `ecp <label>` verb).
fn command_label(cmd: &Commands) -> &'static str {
    match cmd {
        Commands::Inspect(_) => "inspect",
        Commands::Find(_) => "find",
        Commands::Impact(_) => "impact",
        Commands::Rename(_) => "rename",
        Commands::Cypher(_) => "cypher",
        Commands::Routes(_) => "routes",
        Commands::ShapeCheck(_) => "shape-check",
        Commands::ToolMap(_) => "tool-map",
        Commands::Review(_) => "review",
        Commands::FindTransactionPatterns(_) => "find-transaction-patterns",
        Commands::FindSchemaBindings(_) => "find-schema-bindings",
        Commands::FindEventMirrors(_) => "find-event-mirrors",
        Commands::Processes(_) => "processes",
        Commands::Summary(_) => "summary",
        Commands::Contracts(_) => "contracts",
        Commands::Diff(_) => "diff",
        Commands::Admin { .. } => "admin",
        Commands::Dev { .. } => "dev",
        Commands::HookHandle(_) => "hook-handle",
        Commands::HookWatcher(_) => "hook-watcher",
        Commands::Hook(_) => "hook",
        Commands::Watch(_) => "watch",
        Commands::Peers(_) => "peers",
        Commands::Group { .. } => "group",
        Commands::Schema(_) => "schema",
        Commands::Insight(_) => "insight",
        Commands::Usage(_) => "usage",
        Commands::Uninstall(_) => "uninstall",
    }
}
```

2. Move the entire post-`Cli::parse()` dispatch body (everything from the `check_group_atom(&cli);` line through the final match's error handling) into:

```rust
fn dispatch(cli: Cli) -> Result<(), ecp_core::EcpError> { /* moved body */ }
```

Inside `dispatch`, replace every `eprintln!("Command failed: {e}"); std::process::exit(1);` and the graph-load `eprintln!(...); exit(1)` arms with `return Err(e)` / `return Err(...)`. The `run_no_graph!` macro becomes `run_no_graph!($expr) => {{ return $expr.map(|_| ()); }}` (propagate instead of exit). Convert the bare-string graph-load failures (e.g. "Error loading graph from …") into `EcpError::InvalidArgument(format!(...))` so they flow through `Err` and get classified.

3. New `main()` tail (after the subscriber init + `maybe_spawn_background_gc()` + `Cli::parse()`):

```rust
    let cli = Cli::parse();
    let label = command_label(&cli.command);
    let start = std::time::Instant::now();
    let outcome = dispatch(cli);
    let duration_ms = start.elapsed().as_millis() as u64;
    ecp_cli::telemetry_cli::record(label, duration_ms, outcome.as_ref().err());
    if let Err(e) = outcome {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
```

> NOTE: `dispatch` takes `cli` by value (it already consumes `cli.command` via the moving match). `command_label` borrows before the move. If the borrow checker objects, compute `label` from `&cli.command` before calling `dispatch(cli)` — the snippet above already does this.

- [ ] **Step 4: Build, then run the test**

Run: `cargo build -p egent-code-plexus --bin ecp` then `cargo test -p egent-code-plexus --test usage_cmd invocation_appends`
Expected: build OK; test PASS (one `source:"cli"` line written).

- [ ] **Step 5: Sanity — existing CLI tests still pass**

Run: `cargo test -p egent-code-plexus --test cli_surface_invariants`
Expected: PASS (the refactor preserved behavior).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/main.rs crates/ecp-cli/tests/usage_cmd.rs
git commit -m "feat(cli): single recorded dispatch exit point; instrument all invocations"
```

---

## Task 6: `ecp usage` command skeleton + clap wiring + aggregation

**Files:**
- Create: `crates/ecp-cli/src/commands/usage.rs`
- Modify: `crates/ecp-cli/src/commands/mod.rs`, `crates/ecp-cli/src/cli.rs`, `crates/ecp-cli/src/main.rs`

**Context:** Aggregation reuses the shape from `insight.rs` (`read_window`, `aggregate_by_tool`, `percentile`). `usage` reads BOTH `cli-calls.jsonl` and `calls.jsonl`, parses the superset record (with `source`/`error_kind`), and produces a `GainReport` struct that the renderer (Task 7) consumes. This task wires the command end-to-end with JSON output only; text rendering is Task 7.

- [ ] **Step 1: Write the failing test**

Add to `crates/ecp-cli/tests/usage_cmd.rs`:

```rust
#[test]
fn gain_json_aggregates_a_fixture() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-json-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/myrepo__deadbeef");
    std::fs::create_dir_all(&tel).unwrap();
    let lines = [
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:01:00Z","tool":"inspect","duration_ms":48,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:02:00Z","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}"#,
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--format", "json", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["total"], 3);
    let by = v["by_command"].as_array().unwrap();
    let inspect = by.iter().find(|c| c["cmd"] == "inspect").unwrap();
    assert_eq!(inspect["count"], 2);
    assert_eq!(v["errors_by_kind"]["cypher-parse"], 1);
    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test usage_cmd gain_json`
Expected: FAIL — `usage` is not a subcommand.

- [ ] **Step 3: Implement `usage.rs` (args + aggregation + JSON).**

```rust
//! `ecp usage` — human-facing usage dashboard over CLI + MCP telemetry.
//!
//! Reads `cli-calls.jsonl` (+ MCP `calls.jsonl`) for a repo (or all repos),
//! aggregates invocation counts, p50/p99 latency, error rate, and per-kind
//! error tallies. Default output is a terminal ASCII dashboard (Task 7);
//! `--format json` emits the machine-readable shape below. Pruning of lines
//! older than the retention window happens here (off the hot path).

use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::registry::resolve_home_ecp;
use ecp_core::time::parse_rfc3339_secs;
use ecp_core::EcpError;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const BUDGET_MS: u64 = 30;

#[derive(Args, Debug, Clone)]
pub struct UsageArgs {
    /// Scope to the current repository only (default: all repos).
    #[arg(short = 'p', long)]
    pub project: bool,
    /// Show recent failing commands with messages instead of the dashboard.
    #[arg(long)]
    pub failures: bool,
    /// Show the full per-subcommand table (no truncation).
    #[arg(long)]
    pub all: bool,
    /// Output format: text (default) or json.
    #[arg(long, default_value = "text")]
    pub format: Option<String>,
    /// Force color off (also honored: NO_COLOR env, non-TTY stdout).
    #[arg(long)]
    pub no_color: bool,
    /// Hidden: read a single explicit telemetry dir (tests).
    #[arg(long, hide = true)]
    pub telemetry_dir: Option<PathBuf>,
}

pub struct Rec {
    pub ts_secs: u64,
    pub tool: String,
    pub duration_ms: u64,
    pub ok: bool,
    pub source: String,
    pub error_kind: Option<String>,
    pub raw: String,
}

pub fn run(args: UsageArgs) -> Result<(), EcpError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let recs = collect_records(&args)?;
    if matches!(format, OutputFormat::Json) {
        return emit(&build_json(&recs), format);
    }
    // text path → Task 7 renderer
    let want_color = crate::commands::usage_render::color_enabled(&args, &format);
    let text = if args.failures {
        crate::commands::usage_render::render_failures(&recs, want_color)
    } else {
        crate::commands::usage_render::render_dashboard(&recs, want_color, args.all)
    };
    println!("{text}");
    Ok(())
}

/// Telemetry dirs to scan. Explicit `--telemetry-dir` wins; else `-p` →
/// current repo's dir; else every `~/.ecp/telemetry/*`.
fn scan_dirs(args: &UsageArgs) -> Result<Vec<PathBuf>, EcpError> {
    if let Some(d) = &args.telemetry_dir {
        return Ok(vec![d.clone()]);
    }
    let root = resolve_home_ecp().join("telemetry");
    if args.project {
        let cwd = std::env::current_dir()
            .map_err(|e| EcpError::InvalidArgument(format!("cwd: {e}")))?;
        let key = crate::repo_identity::repo_dir_name_for_cwd(&cwd)
            .map_err(|e| EcpError::InvalidArgument(format!("repo identity: {e}")))?;
        return Ok(vec![root.join(key)]);
    }
    let mut dirs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                dirs.push(e.path());
            }
        }
    }
    Ok(dirs)
}

fn collect_records(args: &UsageArgs) -> Result<Vec<Rec>, EcpError> {
    let mut recs = Vec::new();
    for dir in scan_dirs(args)? {
        for name in ["cli-calls.jsonl", "calls.jsonl"] {
            read_file(&dir.join(name), &mut recs);
        }
    }
    Ok(recs)
}

fn read_file(path: &Path, out: &mut Vec<Rec>) {
    let Ok(file) = std::fs::File::open(path) else { return };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(ts) = v.get("ts").and_then(Value::as_str) else { continue };
        out.push(Rec {
            ts_secs: parse_rfc3339_secs(ts).unwrap_or(0),
            tool: v.get("tool").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(true),
            // old MCP lines lack source → default "mcp"
            source: v.get("source").and_then(Value::as_str).unwrap_or("mcp").to_string(),
            error_kind: v.get("error_kind").and_then(Value::as_str).map(str::to_string),
            raw: line.to_string(),
        });
    }
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * pct) / 100]
}

pub struct CmdStat {
    pub cmd: String,
    pub count: usize,
    pub p50: u64,
    pub p99: u64,
    pub errors: usize,
}

/// Per-command aggregation, sorted by count desc. Shared by JSON + renderer.
pub fn by_command(recs: &[Rec]) -> Vec<CmdStat> {
    let mut durs: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut errs: BTreeMap<&str, usize> = BTreeMap::new();
    for r in recs {
        durs.entry(&r.tool).or_default().push(r.duration_ms);
        if !r.ok {
            *errs.entry(&r.tool).or_default() += 1;
        }
    }
    let mut stats: Vec<CmdStat> = durs
        .into_iter()
        .map(|(cmd, mut d)| {
            d.sort_unstable();
            CmdStat {
                cmd: cmd.to_string(),
                count: d.len(),
                p50: percentile(&d, 50),
                p99: percentile(&d, 99),
                errors: *errs.get(cmd).unwrap_or(&0),
            }
        })
        .collect();
    stats.sort_by(|a, b| b.count.cmp(&a.count));
    stats
}

pub fn errors_by_kind(recs: &[Rec]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in recs.iter().filter(|r| !r.ok) {
        let k = r.error_kind.clone().unwrap_or_else(|| "other".to_string());
        *m.entry(k).or_default() += 1;
    }
    m
}

fn build_json(recs: &[Rec]) -> Value {
    let total = recs.len();
    let errors = recs.iter().filter(|r| !r.ok).count();
    let within = recs.iter().filter(|r| r.duration_ms <= BUDGET_MS).count();
    let by_command: Vec<Value> = by_command(recs)
        .iter()
        .map(|s| {
            let er = if s.count > 0 { (s.errors as f64 / s.count as f64 * 10000.0).round() / 10000.0 } else { 0.0 };
            json!({"cmd": s.cmd, "count": s.count, "p50_ms": s.p50, "p99_ms": s.p99, "err_rate": er})
        })
        .collect();
    let mut ebk = Map::new();
    for (k, n) in errors_by_kind(recs) {
        ebk.insert(k, json!(n));
    }
    let err_rate = if total > 0 { (errors as f64 / total as f64 * 10000.0).round() / 10000.0 } else { 0.0 };
    let within_pct = if total > 0 { (within as f64 / total as f64 * 10000.0).round() / 10000.0 } else { 0.0 };
    json!({
        "total": total,
        "error_rate": err_rate,
        "by_command": by_command,
        "errors_by_kind": Value::Object(ebk),
        "within_budget_pct": within_pct
    })
}
```

> NOTE: `usage_render` is created in Task 7; for THIS task, add a temporary stub module so it compiles — create `crates/ecp-cli/src/commands/usage_render.rs` with just:
> ```rust
> use super::usage::Rec;
> use crate::output::OutputFormat;
> pub fn color_enabled(_a: &super::usage::UsageArgs, _f: &OutputFormat) -> bool { false }
> pub fn render_dashboard(_r: &[Rec], _c: bool, _all: bool) -> String { String::new() }
> pub fn render_failures(_r: &[Rec], _c: bool) -> String { String::new() }
> ```
> Task 7 replaces the stub bodies.

- [ ] **Step 4: Wire the command.**
  - `crates/ecp-cli/src/commands/mod.rs`: add `pub mod usage;` and `pub mod usage_render;`.
  - `crates/ecp-cli/src/cli.rs`: add `Gain(commands::usage::UsageArgs),` to the `Commands` enum (place near `Insight`).
  - `crates/ecp-cli/src/main.rs`: in the no-graph `match &cli.command` block, add `Commands::Usage(args) => run_no_graph!(commands::usage::run(args.clone())),`. Add `Commands::Usage(_)` to the two `unreachable!("handled before graph load")` arms (the `repo_opt` match and the final match) and to any other exhaustive `Commands` match the compiler flags.

- [ ] **Step 5: Build + run the test**

Run: `cargo build -p egent-code-plexus --bin ecp` then `cargo test -p egent-code-plexus --test usage_cmd gain_json`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/commands/usage.rs crates/ecp-cli/src/commands/usage_render.rs crates/ecp-cli/src/commands/mod.rs crates/ecp-cli/src/cli.rs crates/ecp-cli/src/main.rs
git commit -m "feat(cli): ecp usage command — aggregation + json output"
```

---

## Task 7: ASCII dashboard renderer + color detection + `--failures`

**Files:**
- Modify: `crates/ecp-cli/src/commands/usage_render.rs` (replace the stub)

**Context:** Replace the Task-6 stub with the real renderer. Sections in order Usage → Performance → Errors, matching spec §4.1. Color via `std::io::IsTerminal` three-state rule (spec §5). Sparkline is per-day buckets; degrades to blank with <2 days.

- [ ] **Step 1: Write the failing test**

Add to `crates/ecp-cli/tests/usage_cmd.rs`:

```rust
#[test]
fn gain_text_dashboard_is_plain_when_piped() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-txt-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__1");
    std::fs::create_dir_all(&tel).unwrap();
    std::fs::write(
        tel.join("cli-calls.jsonl"),
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
    ).unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    // captured stdout is non-TTY → no ANSI escapes
    assert!(!s.contains('\x1b'), "piped output must be color-free");
    assert!(s.contains("Usage"));
    assert!(s.contains("inspect"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn gain_failures_lists_only_errors() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-fail-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__2");
    std::fs::create_dir_all(&tel).unwrap();
    let lines = [
        r#"{"ts":"2026-05-27T07:00:00Z","tool":"inspect","duration_ms":6,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2026-05-27T07:02:00Z","tool":"cypher","duration_ms":9,"ok":false,"source":"cli","error_kind":"cypher-parse"}"#,
    ].join("\n");
    std::fs::write(tel.join("cli-calls.jsonl"), lines).unwrap();
    let out = Command::new(ecp_bin())
        .args(["usage", "--failures", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("cypher-parse"));
    assert!(!s.contains("inspect"), "failures view must omit successful commands");
    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test usage_cmd gain_text gain_failures`
Expected: FAIL — stub returns empty string (no "Usage"/"inspect").

- [ ] **Step 3: Replace the stub** in `crates/ecp-cli/src/commands/usage_render.rs`:

```rust
//! ASCII rendering for `ecp usage`. Sections: Usage → Performance → Errors.
//! Color is opt-in and auto-disabled off a TTY / under NO_COLOR / for json.

use super::usage::{by_command, errors_by_kind, UsageArgs, Rec};
use crate::output::OutputFormat;
use std::fmt::Write as _;
use std::io::IsTerminal;

const BUDGET_MS: u64 = 30;
const BAR_W: usize = 12;

// ANSI (only emitted when color is on).
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Three-state rule (spec §5): --no-color/NO_COLOR off; json off; non-TTY off;
/// else on.
pub fn color_enabled(args: &UsageArgs, format: &OutputFormat) -> bool {
    if args.no_color || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if !matches!(format, OutputFormat::Text) {
        return false;
    }
    std::io::stdout().is_terminal()
}

fn paint(s: &str, color: &str, on: bool) -> String {
    if on {
        format!("{color}{s}{RESET}")
    } else {
        s.to_string()
    }
}

fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn render_dashboard(recs: &[Rec], color: bool, show_all: bool) -> String {
    let mut o = String::new();
    let total = recs.len();
    if total == 0 {
        return "ecp Usage Dashboard\n  (no telemetry yet — run some ecp commands first)".to_string();
    }
    let errors = recs.iter().filter(|r| !r.ok).count();
    let within = recs.iter().filter(|r| r.duration_ms <= BUDGET_MS).count();
    let cli_n = recs.iter().filter(|r| r.source == "cli").count();
    let mcp_n = total - cli_n;
    let mut all_durs: Vec<u64> = recs.iter().map(|r| r.duration_ms).collect();
    all_durs.sort_unstable();
    let p50 = super::usage_render::pctl(&all_durs, 50);
    let p99 = super::usage_render::pctl(&all_durs, 99);
    let err_pct = errors as f64 / total as f64 * 100.0;

    let _ = writeln!(o, "ecp Usage Dashboard");
    let _ = writeln!(o, "{}", "═".repeat(76));
    let _ = writeln!(o, " Total invocations  {total:<10}  Error rate  {err_pct:.1}%  ({errors} failed)");
    let _ = writeln!(o, " Median latency     {p50:<3}ms      p99 {p99}ms     Sources  cli {cli_n} · mcp {mcp_n}");
    let _ = writeln!(o);

    // ── Usage ──
    let _ = writeln!(o, "▌Usage  (by subcommand)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(o, "  #  Command                      Count   Share   p50    p99   Err%");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let stats = by_command(recs);
    let max_count = stats.first().map(|s| s.count).unwrap_or(1).max(1);
    let shown = if show_all { stats.len() } else { stats.len().min(10) };
    for (i, s) in stats.iter().take(shown).enumerate() {
        let share = s.count as f64 / total as f64 * 100.0;
        let er = if s.count > 0 { s.errors as f64 / s.count as f64 * 100.0 } else { 0.0 };
        let er_cell = if er >= 5.0 {
            paint(&format!("{er:>4.1}% !"), RED, color)
        } else {
            format!("{er:>4.1}%  ")
        };
        let b = bar(s.count as f64 / max_count as f64, BAR_W);
        let _ = writeln!(
            o,
            "  {:<2} {:<22} {:>6}  {:>5.1}%  {:>3}ms {:>4}ms  {}  {}",
            i + 1, s.cmd, s.count, share, s.p50, s.p99, er_cell, b
        );
    }
    if !show_all && stats.len() > shown {
        let _ = writeln!(o, "  …  ({} more — see --all)", stats.len() - shown);
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(o);

    // ── Performance ──
    let _ = writeln!(o, "▌Performance  (latency budget: <{BUDGET_MS}ms target)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let within_frac = within as f64 / total as f64;
    let over = total - within;
    let within_lbl = paint("Within budget", GREEN, color);
    let over_lbl = paint("Over budget  ", if within_frac < 0.7 { RED } else { YELLOW }, color);
    let _ = writeln!(o, "  {within_lbl}  {}  {:.1}%  ({within})", bar(within_frac, 24), within_frac * 100.0);
    let _ = writeln!(o, "  {over_lbl}  {}  {:.1}%  ({over})", bar(1.0 - within_frac, 24), (1.0 - within_frac) * 100.0);
    let _ = writeln!(o, "{}", "─".repeat(76));
    let _ = writeln!(o);

    // ── Errors ──
    let _ = writeln!(o, "▌Errors  ({errors} total · {err_pct:.1}%)");
    let _ = writeln!(o, "{}", "─".repeat(76));
    let ebk = errors_by_kind(recs);
    let max_e = ebk.values().copied().max().unwrap_or(1).max(1);
    if ebk.is_empty() {
        let _ = writeln!(o, "  (none)");
    } else {
        let mut pairs: Vec<(String, usize)> = ebk.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1));
        for (kind, n) in pairs {
            let _ = writeln!(o, "  {:<20} {:>4}  {}", kind, n, bar(n as f64 / max_e as f64, 16));
        }
        let _ = writeln!(o, "  Tip: ecp usage --failures   for recent failing commands + messages");
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    o
}

pub fn render_failures(recs: &[Rec], _color: bool) -> String {
    let fails: Vec<&Rec> = recs.iter().filter(|r| !r.ok).collect();
    let mut o = String::new();
    let _ = writeln!(o, "ecp Failures  (recent {} of {})", fails.len().min(20), fails.len());
    let _ = writeln!(o, "{}", "═".repeat(76));
    for r in fails.iter().rev().take(20) {
        let kind = r.error_kind.as_deref().unwrap_or("other");
        let _ = writeln!(o, "  {}  {:<14} {}", r.ts_secs, r.tool, kind);
        let _ = writeln!(o, "       └ {}", r.raw.chars().take(100).collect::<String>());
    }
    let _ = writeln!(o, "{}", "─".repeat(76));
    o
}

/// Shared percentile (usage.rs's is private; expose one here for the renderer).
pub fn pctl(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * pct) / 100]
}
```

> NOTE: the `super::usage_render::pctl` / `super::usage_render::` self-paths in `render_dashboard` are just `pctl(...)` (same module) — drop the `super::usage_render::` prefix when implementing; it's shown qualified only to name the function. Remove the now-unused `color_enabled` import of `Rec` if the compiler warns.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p egent-code-plexus --test usage_cmd`
Expected: all `gain_*` tests PASS (json, text-plain, failures, invocation).

- [ ] **Step 5: Eyeball it** (human-visible sanity, color path)

Run: `cargo run -p egent-code-plexus --bin ecp -- usage` (in a repo that has telemetry)
Expected: a three-section dashboard; if run in a real terminal, budget bars are colored.

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/commands/usage_render.rs crates/ecp-cli/tests/usage_cmd.rs
git commit -m "feat(cli): ecp usage ASCII dashboard + failures view + color detection"
```

---

## Task 8: Retention prune (off the hot path) + GC fallback

**Files:**
- Modify: `crates/ecp-cli/src/commands/usage.rs` (prune on read)
- Modify: `crates/ecp-cli/src/commands/admin/gc.rs` (fallback prune)

**Context:** Append never prunes. `ecp usage` prunes when it runs (it reads the whole file anyway). `gc.rs:44` currently exempts the whole `telemetry/` dir — narrow that so MCP `calls.jsonl` stays exempt but `cli-calls.jsonl` gets `retention_days` pruning.

- [ ] **Step 1: Write the failing test**

Add to `crates/ecp-cli/tests/usage_cmd.rs`:

```rust
#[test]
fn gain_prunes_lines_older_than_retention() {
    let tmp = std::env::temp_dir().join(format!("ecp-usage-prune-{}", std::process::id()));
    let tel = tmp.join(".ecp/telemetry/r__3");
    std::fs::create_dir_all(&tel).unwrap();
    // one ancient line (2020) + one fresh line (far-future so it survives any window)
    let lines = [
        r#"{"ts":"2020-01-01T00:00:00Z","tool":"find","duration_ms":4,"ok":true,"source":"cli","error_kind":null}"#,
        r#"{"ts":"2099-01-01T00:00:00Z","tool":"find","duration_ms":4,"ok":true,"source":"cli","error_kind":null}"#,
    ].join("\n");
    let f = tel.join("cli-calls.jsonl");
    std::fs::write(&f, lines).unwrap();
    // usage with explicit dir + project-agnostic; retention default 7 days prunes 2020 line
    let _ = Command::new(ecp_bin())
        .args(["usage", "--format", "json", "--telemetry-dir", tel.to_str().unwrap()])
        .output()
        .unwrap();
    let body = std::fs::read_to_string(&f).unwrap();
    assert!(!body.contains("2020-01-01"), "ancient line must be pruned");
    assert!(body.contains("2099-01-01"), "fresh line must survive");
    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p egent-code-plexus --test usage_cmd gain_prunes`
Expected: FAIL — 2020 line still present (no prune).

- [ ] **Step 3: Implement prune in `usage.rs`.** Add and call it from `collect_records` after reading each `cli-calls.jsonl`:

```rust
/// Rewrite `cli-calls.jsonl` dropping lines older than `retention_days`.
/// Off the hot path: only `ecp usage` (and GC) call this. Best-effort.
fn prune_retention(dir: &Path) {
    let days = crate::config_parser::load_effective_config()
        .map(|c| c.telemetry.retention_days)
        .unwrap_or(7);
    let cutoff = now_unix_secs().saturating_sub(days * 86400);
    let path = dir.join("cli-calls.jsonl");
    let Ok(body) = std::fs::read_to_string(&path) else { return };
    let kept: Vec<&str> = body
        .lines()
        .filter(|l| {
            serde_json::from_str::<Value>(l)
                .ok()
                .and_then(|v| v.get("ts").and_then(Value::as_str).map(str::to_string))
                .and_then(|ts| parse_rfc3339_secs(&ts))
                .map(|secs| secs >= cutoff)
                .unwrap_or(true) // keep unparseable lines
        })
        .collect();
    if kept.len() != body.lines().count() {
        let _ = std::fs::write(&path, kept.join("\n") + "\n");
    }
}

fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
```

In `collect_records`, before/after reading each dir, call `prune_retention(&dir);` (call it once per dir). Note: `--telemetry-dir` test path uses the default retention (7d), which is why the 2020 line is pruned.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p egent-code-plexus --test usage_cmd gain_prunes`
Expected: PASS.

- [ ] **Step 5: GC fallback.** In `crates/ecp-cli/src/commands/admin/gc.rs`, find the `telemetry` exemption (`grep -n "telemetry" crates/ecp-cli/src/commands/admin/gc.rs`). Change it so the `telemetry/` dir is still walked but, for each `<repo>/cli-calls.jsonl`, call the same retention logic. Extract `prune_retention` into a `pub(crate)` fn reachable from gc (or duplicate the ~12-line filter — prefer extracting to `usage.rs` as `pub(crate) fn prune_retention`). Keep `calls.jsonl` (MCP) exempt.

- [ ] **Step 6: Build + full usage test pass**

Run: `cargo build -p egent-code-plexus --bin ecp` then `cargo test -p egent-code-plexus --test usage_cmd`
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ecp-cli/src/commands/usage.rs crates/ecp-cli/src/commands/admin/gc.rs crates/ecp-cli/tests/usage_cmd.rs
git commit -m "feat(cli): 7-day retention prune on gain-read + gc fallback"
```

---

## Task 9: Hot-path verification + lint + docs

**Files:**
- Modify: `crates/ecp-cli/src/main.rs` (only if benchmark reveals regression)

- [ ] **Step 1: Benchmark telemetry on vs off.**

Run:
```bash
cargo build -p egent-code-plexus --bin ecp --release
python scripts/benchmark/benchmark_ecp.py 2>&1 | tee /tmp/gain-bench-on.txt
ECP_NO_TELEMETRY=1 python scripts/benchmark/benchmark_ecp.py 2>&1 | tee /tmp/gain-bench-off.txt
```
Expected: per-query p50 delta within run-to-run noise (the append is one buffered `writeln` per process, no fsync). If p50 regresses measurably, the append is on a hotter path than expected — investigate before proceeding (do NOT claim the perf win without this evidence, per CLAUDE.md).

- [ ] **Step 2: Lint.**

Run: `cargo clippy -p egent-code-plexus --tests && cargo clippy -p ecp-core && cargo clippy -p ecp-mcp`
Expected: no warnings on touched files. Fix any.

- [ ] **Step 3: Format touched files only.**

Run: `rustfmt --edition 2021 crates/ecp-core/src/telemetry.rs crates/ecp-cli/src/telemetry_cli.rs crates/ecp-cli/src/commands/usage.rs crates/ecp-cli/src/commands/usage_render.rs crates/ecp-cli/src/main.rs`

- [ ] **Step 4: Full relevant test sweep.**

Run: `cargo test -p egent-code-plexus --tests && cargo test -p ecp-core telemetry`
Expected: PASS.

- [ ] **Step 5: Help-text sanity.**

Run: `cargo run -p egent-code-plexus --bin ecp -- usage --help`
Expected: shows `-p/--project`, `--failures`, `--all`, `--format`, `--no-color`; hides `--telemetry-dir`.

- [ ] **Step 6: Commit.**

```bash
git add -A
git commit -m "chore(gain): hot-path bench evidence, clippy clean, fmt"
```

---

## Self-Review Notes (for the executor)

- **Spec coverage**: §3.1 shared CallRecord → T1/T2; §3.2 hot-path-safe collection → T4/T5/T9; §3.3 taxonomy → T4; §4 presentation (dashboard/failures/json/flags) → T6/T7; §5 color → T7; §6 retention → T8 + bench T9; §7 testing → tests in every task; §2 positioning (top-level, opt-out) → T3/T6.
- **Open decision §8 (crate home)**: resolved → ecp-core (T1), because telemetry.rs already depends only on ecp-core (`time`, `registry`).
- **Open decision §8 (cli/mcp split row)**: resolved → summary line only (T7 header shows `Sources cli N · mcp N`), no per-row split.
- **Sparkline note**: the spec's per-day sparkline (§4.1) is the lowest-priority visual. Tasks 6–7 ship the dashboard WITHOUT the sparkline column to keep the renderer focused; the `Trend` column is a fast-follow once the core is green (tracked as a follow-up, not a blocker). If the executor has budget, add a per-day bucket + `▁▃▅█` mapping in `render_dashboard` and a test asserting it degrades to blank with <2 distinct days.
- **PR discipline**: per global CLAUDE.md, run `/simplify` before pushing; open the PR from a dedicated `feat/ecp-usage-dashboard` branch (the worktree branch). No `Co-Authored-By` / "Generated with" trailers. Read `.claude/FOLLOWUPS.md` Open section before opening the PR; file the sparkline deferral as a new FU entry.
