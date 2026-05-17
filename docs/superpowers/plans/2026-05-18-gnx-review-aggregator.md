# `gnx review` Aggregator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace 6 individual CLI commands (`scan`, `impact`, `coverage`, `tool-map`, `shape-check`, `diff`) with a single `gnx review` aggregator. `scan` is hard-deleted (signal:noise too low per spec). The other 5 become Rust-library-only and are orchestrated from `review`.

**Architecture:**
- Each constituent (`impact` / `coverage` / `shape_check` / `tool_map`→`egress` / `diff`) is split into two layers: a `build_payload(args, engine) -> Result<Value, GnxError>` library fn (pure data, no clap, no emit) and (Phase 1) a thin CLI wrapper that calls it then emits. Phase 3 removes the CLI wrappers; the library fns stay.
- `commands::review::{scope, aggregate, findings, mod}` orchestrates: resolve change-set → per-file dispatch into each constituent's `build_payload` → map payloads to `Finding` rows → filter to high-confidence → emit per spec schema.
- MCP exposure follows CLI automatically (`schema.rs::enumerate_tools` reflects from clap `Command` tree). No manual MCP code changes needed.

**Tech Stack:** Rust 2021, clap 4, serde_json, existing `Engine`/`ZeroCopyGraph`/`OutputFormat`, `ShellGitProvider` for diff/since resolution.

**Spec:** `docs/feat/2026-05-17-gnx-review-spec.md`

---

## Scope check & landing strategy

This plan spans 4 logically independent phases. Recommended: land each as its own PR, in order. If the user prefers one big PR, run all phases sequentially in this worktree before opening the PR.

| Phase | Subject | Risk | Reverts cleanly? |
|---|---|---|---|
| 0 | Hard-delete `gnx scan` + `scan_filters` + tests | low | yes |
| 1 | Refactor each constituent into `build_payload` library fn | low (pure refactor) | yes |
| 2 | New `gnx review` aggregator (additive) | medium (new code) | yes |
| 3 | Remove 5 standalone CLI variants + rename tool_map → egress | high (breaking) | hard |

**Pre-flight (every phase):**
```bash
cargo build -p graph-nexus --bin gnx --release
cargo test -p graph-nexus --tests
cargo clippy -p graph-nexus --tests -- -D warnings
```

---

## Phase 0 — Remove `gnx scan`

### Task 0.1: Inventory scan touchpoints

**Files:** (no edits — info gathering)

- [ ] **Step 1: Enumerate scan references**

```bash
rg -ln '\bscan\b' crates/ docs/ README*.md .claude/ tests/ 2>/dev/null
```

Expected: ~10 files including
- `crates/graph-nexus-cli/src/commands/scan.rs`
- `crates/graph-nexus-cli/src/commands/scan_filters.rs`
- `crates/graph-nexus-cli/tests/scan_cmd.rs`
- `crates/graph-nexus-cli/src/main.rs`
- `crates/graph-nexus-cli/src/commands/mod.rs`
- `crates/graph-nexus-cli/tests/cli_surface.rs`
- `crates/graph-nexus-cli/tests/cli_help_surface_test.rs`
- README / docs hits

Filter out false positives ("scan" inside code comments unrelated to the command).

- [ ] **Step 2: Inspect cli_surface test to know what assertions to update**

```bash
rg -n '"scan"|scan_filters' crates/graph-nexus-cli/tests/
```

### Task 0.2: Failing test that confirms scan is gone

**Files:**
- Modify: `crates/graph-nexus-cli/tests/cli_surface.rs`

- [ ] **Step 1: Add a regression test that `gnx --help` does NOT list scan**

Append below the existing `no_old_top_level_commands` test:

```rust
#[test]
fn top_level_does_not_list_scan() {
    let help = gnx_help();
    for line in help.lines() {
        let t = line.trim_start();
        // scan is removed; allow the word "scan" in unrelated descriptions
        // but not as a subcommand entry (line starts with "scan " or "scan\t").
        if t.starts_with("scan ") || t.starts_with("scan\t") {
            panic!("scan command leaked into --help: {line}");
        }
    }
}
```

Also remove `"scan"` from the `top_level_lists_nine_agent_commands` array (the test name will be retouched after Phase 3; for now just delete the array entry).

- [ ] **Step 2: Run the new test — must FAIL because scan still exists**

```bash
cargo test -p graph-nexus --test cli_surface top_level_does_not_list_scan
```

Expected: panic with `scan command leaked into --help`.

### Task 0.3: Remove scan source files

**Files:**
- Delete: `crates/graph-nexus-cli/src/commands/scan.rs`
- Delete: `crates/graph-nexus-cli/src/commands/scan_filters.rs`
- Delete: `crates/graph-nexus-cli/tests/scan_cmd.rs`
- Delete: any other `crates/graph-nexus-cli/tests/scan_*` files found in Task 0.1

- [ ] **Step 1: Delete files**

```bash
git rm crates/graph-nexus-cli/src/commands/scan.rs \
       crates/graph-nexus-cli/src/commands/scan_filters.rs \
       crates/graph-nexus-cli/tests/scan_cmd.rs
# Repeat for any extras from Task 0.1
```

### Task 0.4: Unwire scan from main.rs

**Files:**
- Modify: `crates/graph-nexus-cli/src/main.rs:69` (variant), `:165` (repo dispatch), `:204` (run dispatch)

- [ ] **Step 1: Remove the `Scan` variant**

In the `enum Commands` block, delete:
```rust
    /// Verify a file's symbol references exist in the graph
    Scan(commands::scan::ScanArgs),
```

- [ ] **Step 2: Remove the repo dispatch arm**

In the `let repo_opt = match ...` block, delete:
```rust
        Commands::Scan(args) => args.repo.as_deref(),
```

- [ ] **Step 3: Remove the run dispatch arm**

In the final `match cli.command` block, delete:
```rust
        Commands::Scan(args) => commands::scan::run(args, &engine),
```

### Task 0.5: Unwire scan from commands/mod.rs

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs`

- [ ] **Step 1: Delete the `pub mod scan;` and `pub mod scan_filters;` lines**

```bash
rg -n '^pub mod scan' crates/graph-nexus-cli/src/commands/mod.rs
```

Delete both lines. Verify no other internal callers (analyzer crate doesn't depend on CLI, but other CLI modules might import `scan_filters` — grep first):

```bash
rg -n 'use crate::commands::scan' crates/graph-nexus-cli/src/
```

### Task 0.6: Decide on `identifier_finder/python.rs` POC

**Context:** The original spec said "Python POC import-aware code stays in the tree as a reference implementation." This POC currently has no callers (scan was its only consumer). It is dead code.

- [ ] **Step 1: Check whether anything still uses it**

```bash
rg -n 'identifier_finder' crates/ --type rust
```

If only `crates/graph-nexus-analyzer/src/lib.rs` exports it and `crates/graph-nexus-cli/src/commands/scan.rs` consumed it, the entire module becomes orphaned by Phase 0.

- [ ] **Step 2: Resolution (per CLAUDE.md "no dead code" rule overrides spec's "keep as reference")**

Action: delete `crates/graph-nexus-analyzer/src/identifier_finder/` directory and its `pub mod identifier_finder;` line in the analyzer crate's `lib.rs`. The git history is the reference; tree-bloat is not.

If anything outside scan/scan_filters does reference it, keep the module and only delete what's truly orphaned.

### Task 0.7: Sweep docs / skills / README

**Files:** discovered in Task 0.1 (excluding `docs/feat/2026-05-17-gnx-review-spec.md` — that's the spec, leave it).

- [ ] **Step 1: For each doc file, remove `gnx scan` mentions, replace with brief deprecation note in the relevant section**

Search:
```bash
rg -n 'gnx scan|gnx_scan' docs/ README*.md .claude/skills/ 2>/dev/null
```

For each hit: delete the line/paragraph that documents scan as a usable command. If a doc lists "available subcommands", remove `scan` from that list. Don't add backwards-compat redirect text — the command is gone, not renamed.

### Task 0.8: Build + run + commit

- [ ] **Step 1: Build**

```bash
cargo build -p graph-nexus --bin gnx --release
```

Expected: clean build.

- [ ] **Step 2: Run the previously-failing test — must PASS now**

```bash
cargo test -p graph-nexus --test cli_surface top_level_does_not_list_scan
```

Expected: PASS.

- [ ] **Step 3: Full test suite**

```bash
cargo test -p graph-nexus --tests
cargo test -p graph-nexus-analyzer
```

Expected: all green. If anything still references scan, fix it.

- [ ] **Step 4: Clippy**

```bash
cargo clippy -p graph-nexus --tests -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(cli): hard-delete gnx scan + scan_filters + identifier_finder

Signal:noise was ~1:10 even with stdlib filters; language-native
linters cover the actionable subset. Removed:
- gnx scan subcommand (CLI + MCP via clap auto-derive)
- scan_filters module
- identifier_finder analyzer module (sole consumer was scan)
- scan_cmd / scan_filters tests

Spec: docs/feat/2026-05-17-gnx-review-spec.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase 1 — Library extraction for the 5 review constituents

For each constituent (`impact`, `coverage`, `tool_map`, `shape_check`, `diff`):
- Add `pub fn build_payload(args: &XxxArgs, engine: &Engine) -> Result<serde_json::Value, GnxError>` (pure — no `emit`, no `println!`)
- Keep `pub fn run(args: XxxArgs, engine: &Engine) -> Result<(), GnxError>` as a 3-line wrapper:
  ```rust
  let payload = build_payload(&args, engine)?;
  let format = OutputFormat::parse(args.format.as_deref());
  emit(&payload, format)
  ```
- All existing tests must pass unchanged.
- For constituents that already have a private `build_payload` (`shape_check`), promote visibility to `pub`.

Strategy: TDD via golden output — add a test that compares `build_payload`'s `Value` output to the result of running the CLI and parsing its JSON output back. Run before refactor (fails: fn doesn't exist), then after (passes).

### Task 1.1: Extract `impact::build_payload`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/impact.rs`
- Test: `crates/graph-nexus-cli/tests/impact_build_payload.rs` (new)

- [ ] **Step 1: Write a golden-payload test using a fixture repo**

```rust
// crates/graph-nexus-cli/tests/impact_build_payload.rs
use graph_nexus_cli::commands::impact::{build_payload, ImpactArgs, Direction};
// (path adjusts to actual crate name `graph_nexus_cli`)

#[test]
fn build_payload_for_known_symbol_returns_value() {
    let engine = test_helpers::load_fixture_engine("simple_python");
    let args = ImpactArgs {
        name: Some("hello".to_string()),
        target: None,
        baseline: None,
        file: None,
        kind: None,
        direction: Direction::Up,
        depth: 5,
        high_trust_only: false,
        min_confidence: None,
        include_tests: false,
        relation_types: None,
        repo: None,
        format: None,
    };
    let v = build_payload(&args, &engine).expect("build_payload");
    assert!(v.get("impact").is_some() || v.get("results").is_some(),
            "payload shape: {v}");
}
```

(If a `test_helpers` module doesn't yet exist for fixture-loading, mirror whatever pattern existing impact tests use — `crates/graph-nexus-cli/tests/impact_*.rs` files.)

- [ ] **Step 2: Run — fails (build_payload undefined)**

```bash
cargo test -p graph-nexus --test impact_build_payload
```

Expected: compile error `cannot find function build_payload`.

- [ ] **Step 3: Refactor `run` to delegate to `build_payload`**

In `impact.rs`:
- Move the body of `run` (after the `args.target` fold-in lines) into a new `pub fn build_payload(args: &mut ImpactArgs, engine: &Engine) -> Result<Value, GnxError>` that returns the final `Value` instead of calling `emit`.
- Replace `emit(&result, format)` and `emit(&value, format)` call sites with `return Ok(result);` / `return Ok(value);`.
- Make `run` become:
  ```rust
  pub fn run(mut args: ImpactArgs, engine: &Engine) -> Result<(), GnxError> {
      if args.name.is_none() && args.target.is_some() {
          args.name = args.target.take();
      }
      let format = OutputFormat::parse(args.format.as_deref());
      let payload = build_payload(&mut args, engine)?;
      emit(&payload, format)
  }
  ```
- The `impact_by_name` / `impact_with_baseline` helper signatures change to return `Result<Value, GnxError>` instead of calling `emit` themselves.

- [ ] **Step 4: Run new test + existing impact tests**

```bash
cargo test -p graph-nexus --test impact_build_payload
cargo test -p graph-nexus --test 'impact*'
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/impact.rs \
        crates/graph-nexus-cli/tests/impact_build_payload.rs
git commit -m "refactor(impact): split build_payload library fn from run wrapper

Preparation for gnx review aggregator (Phase 2). No behavior change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 1.2: Extract `coverage::build_payload`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/coverage.rs`
- Test: `crates/graph-nexus-cli/tests/coverage_build_payload.rs` (new)

- [ ] **Step 1: Failing golden-payload test**

```rust
// crates/graph-nexus-cli/tests/coverage_build_payload.rs
use graph_nexus_cli::commands::coverage::{build_payload, CoverageArgs};
use std::path::PathBuf;

#[test]
fn build_payload_default_returns_indexed_repos_section() {
    let args = CoverageArgs { repo: None, detailed: false, format: None };
    let v = build_payload(&args, &PathBuf::from(".")).expect("build_payload");
    let cov = v.get("coverage").expect("coverage key");
    assert!(cov.get("indexed_repos").is_some(), "shape: {v}");
}
```

- [ ] **Step 2: Run — fails**

- [ ] **Step 3: Refactor — extract `pub fn build_payload(args: &CoverageArgs, _graph_arg: &Path) -> Result<Value, GnxError>` containing everything currently between `let format = OutputFormat::parse(...)` and `emit(&value, format)`. Have `run` call `build_payload` then `emit`.**

- [ ] **Step 4: All coverage tests pass**

```bash
cargo test -p graph-nexus --test 'coverage*'
cargo test -p graph-nexus --test blind_spots_python
```

- [ ] **Step 5: Commit**

### Task 1.3: Extract `shape_check::build_payload` (visibility promotion)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/shape_check.rs:48`

`shape_check.rs` already has `fn build_payload(args: ShapeCheckArgs, engine: &Engine) -> Result<Value, GnxError>`. Just promote to `pub` and change signature to take `&ShapeCheckArgs`.

- [ ] **Step 1: Failing test mirroring 1.1/1.2 pattern**
- [ ] **Step 2: Run — fails**
- [ ] **Step 3: Change `fn build_payload` to `pub fn build_payload`, change `args: ShapeCheckArgs` to `args: &ShapeCheckArgs`; update `run` to pass `&args`.**
- [ ] **Step 4: All shape_check tests pass**
- [ ] **Step 5: Commit**

### Task 1.4: Extract `tool_map::build_payload`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/tool_map.rs:150`

- [ ] **Step 1: Failing test**
- [ ] **Step 2: Run — fails**
- [ ] **Step 3: Mirror Task 1.1 mechanics — extract body of `run` into `pub fn build_payload(args: &ToolMapArgs, engine: &Engine) -> Result<Value, GnxError>`. Skip the `emit` at end; have `run` call `build_payload` then `emit`.**
- [ ] **Step 4: All tool_map tests pass**

```bash
cargo test -p graph-nexus --test 'tool_map*'
```

- [ ] **Step 5: Commit**

### Task 1.5: Extract `diff::build_payload`

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/diff/mod.rs:51`

The diff command emits via its own `output::emit` helper, not the global `OutputFormat::emit`. Structure:
- Extract the part that produces the diff structs (`BindingsDiff` / `RoutesDiff` / `ContractsDiff` triple).
- New library fn: `pub fn build_payload(args: &DiffArgs) -> Result<DiffPayload, GnxError>` where `DiffPayload` is a struct holding the three optional diffs + baseline/current SHAs.
- `run` calls `build_payload` then `output::emit`.

- [ ] **Step 1: Failing test asserting `build_payload` exists and returns the triple**
- [ ] **Step 2: Run — fails**
- [ ] **Step 3: Define `DiffPayload` struct in `diff/mod.rs`. Move all computation in `run` (between `let repo_dir = ...` and the final `output::emit` call) into `build_payload`. `run` becomes 4 lines.**
- [ ] **Step 4: All diff tests pass**

```bash
cargo test -p graph-nexus --test 'diff*'
```

- [ ] **Step 5: Commit**

### Task 1.6: Make `commands` module public for inter-crate-style use

**Files:**
- Modify: `crates/graph-nexus-cli/src/lib.rs` (or `main.rs` if no `lib.rs`)
- Modify: `crates/graph-nexus-cli/Cargo.toml` if needed

The `review` aggregator will live in this same crate, so this is intra-crate. Verify the `commands` module is `pub mod commands;` (it should already be — review `main.rs:17`). No edit needed if so.

If review needs to be importable from external crates (e.g., MCP), revisit; spec doesn't currently call for that.

- [ ] **Step 1: Verify `commands` is `pub mod commands;`**

```bash
rg -n '^pub mod commands' crates/graph-nexus-cli/src/main.rs crates/graph-nexus-cli/src/lib.rs 2>/dev/null
```

If only `mod commands;` (private), change to `pub mod commands;`.

---

## Phase 2 — Build `gnx review` aggregator

### Task 2.1: Create review module skeleton

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/review/mod.rs`
- Create: `crates/graph-nexus-cli/src/commands/review/findings.rs`
- Create: `crates/graph-nexus-cli/src/commands/review/scope.rs`
- Create: `crates/graph-nexus-cli/src/commands/review/aggregate.rs`
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs` — add `pub mod review;`

- [ ] **Step 1: Create empty module files**

```bash
mkdir -p crates/graph-nexus-cli/src/commands/review
```

Write `crates/graph-nexus-cli/src/commands/review/mod.rs`:

```rust
//! `gnx review` — LLM-workflow audit aggregator.
//!
//! One command, one report. Calls each constituent's `build_payload`
//! library fn, maps results to `Finding` rows, filters to high-confidence
//! signal only, emits per spec.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;

pub mod aggregate;
pub mod findings;
pub mod scope;

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    /// Git ref to diff against. Defaults to working-tree changes (HEAD).
    #[arg(long)]
    pub since: Option<String>,

    /// Explicit file list (comma-separated). Overrides --since.
    #[arg(long, value_delimiter = ',')]
    pub files: Option<Vec<String>>,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: toon (default) | json
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: ReviewArgs, engine: &Engine) -> Result<(), GnxError> {
    let start = std::time::Instant::now();
    let repo_dir = args
        .repo
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let files = scope::resolve(&args, &repo_dir)?;
    let report = aggregate::run(&files, &repo_dir, engine)?;
    let payload = report.emit(start.elapsed());
    let format = OutputFormat::parse(args.format.as_deref());
    emit(&payload, format)
}
```

Add `pub mod review;` to `crates/graph-nexus-cli/src/commands/mod.rs`.

### Task 2.2: Findings type

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/review/findings.rs`

- [ ] **Step 1: Write failing unit test inside findings.rs**

```rust
// crates/graph-nexus-cli/src/commands/review/findings.rs
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Warn, Info }

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source { Impact, Egress, ShapeCheck, BlindSpot, Resolver }

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: String,
    pub line: u32,
    pub kind: &'static str,
    pub severity: Severity,
    pub message: String,
    pub source: Source,
}

#[derive(Default, Debug)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub files_reviewed: usize,
}

impl Report {
    pub fn emit(&self, elapsed: std::time::Duration) -> serde_json::Value {
        use std::collections::BTreeMap;
        let mut per_file: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
        for f in &self.findings {
            per_file.entry(f.file.as_str()).or_default().push(f);
        }
        let warn_count = self.findings.iter().filter(|f| f.severity == Severity::Warn).count();
        let info_count = self.findings.iter().filter(|f| f.severity == Severity::Info).count();
        let clean_files = self.files_reviewed.saturating_sub(per_file.len());

        if self.findings.is_empty() {
            return serde_json::json!({
                "status": "clean",
                "files_reviewed": self.files_reviewed,
                "elapsed_ms": elapsed.as_millis() as u64,
            });
        }

        let files: Vec<serde_json::Value> = per_file.into_iter().map(|(path, items)| {
            let rows: Vec<serde_json::Value> = items.iter().map(|f| serde_json::to_value(f).unwrap()).collect();
            serde_json::json!({ "path": path, "findings": rows })
        }).collect();

        serde_json::json!({
            "files": files,
            "summary": {
                "files_reviewed": self.files_reviewed,
                "warn_count": warn_count,
                "info_count": info_count,
                "clean_files": clean_files,
                "elapsed_ms": elapsed.as_millis() as u64,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_with_no_findings_emits_clean_status() {
        let r = Report { findings: vec![], files_reviewed: 3 };
        let v = r.emit(std::time::Duration::from_millis(42));
        assert_eq!(v["status"], "clean");
        assert_eq!(v["files_reviewed"], 3);
        assert_eq!(v["elapsed_ms"], 42);
    }

    #[test]
    fn report_groups_findings_by_file() {
        let r = Report {
            findings: vec![
                Finding { file: "a.rs".into(), line: 1, kind: "impact", severity: Severity::Info, message: "8 callers".into(), source: Source::Impact },
                Finding { file: "a.rs".into(), line: 2, kind: "egress", severity: Severity::Warn, message: "new HTTP call".into(), source: Source::Egress },
                Finding { file: "b.rs".into(), line: 5, kind: "blind_spot", severity: Severity::Info, message: "framework x not in graph".into(), source: Source::BlindSpot },
            ],
            files_reviewed: 2,
        };
        let v = r.emit(std::time::Duration::from_millis(10));
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(v["summary"]["warn_count"], 1);
        assert_eq!(v["summary"]["info_count"], 2);
        assert_eq!(v["summary"]["clean_files"], 0);
    }
}
```

- [ ] **Step 2: Run — tests fail until file exists**

```bash
cargo test -p graph-nexus --lib commands::review::findings
```

- [ ] **Step 3: Code is already written in step 1. Verify tests pass after writing the file.**

```bash
cargo test -p graph-nexus --lib commands::review::findings
```

Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/review/
git commit -m "feat(review): findings type + Report emitter skeleton

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

### Task 2.3: scope.rs — `--since` / `--files` / cwd resolution

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/review/scope.rs`
- Test: tests inside the same file (`#[cfg(test)]` block)

Behavior:
- If `args.files` is `Some(list)` → use list verbatim.
- Else if `args.since` is `Some(ref)` → `git diff <ref>...HEAD --name-only` (use `ShellGitProvider`).
- Else → `git diff HEAD --name-only` plus untracked files (`git ls-files --others --exclude-standard`).

- [ ] **Step 1: Failing test (scope resolution behavior)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_files_override_since() {
        let args = super::super::ReviewArgs {
            since: Some("main".into()),
            files: Some(vec!["a.rs".into(), "b.rs".into()]),
            repo: None,
            format: None,
        };
        // resolve runs `git ...` only when files is None — assert we get
        // the explicit list back without touching git.
        let v = resolve(&args, &PathBuf::from(".")).unwrap();
        assert_eq!(v, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }
}
```

- [ ] **Step 2: Run — fails (resolve undefined)**

- [ ] **Step 3: Implement `pub fn resolve(args: &ReviewArgs, repo_dir: &Path) -> Result<Vec<PathBuf>, GnxError>`**

```rust
use crate::git::ShellGitProvider;
use graph_nexus_core::GnxError;
use std::path::{Path, PathBuf};

pub fn resolve(args: &super::ReviewArgs, repo_dir: &Path) -> Result<Vec<PathBuf>, GnxError> {
    if let Some(files) = &args.files {
        return Ok(files.iter().map(PathBuf::from).collect());
    }
    let provider = ShellGitProvider::new();
    let names = match &args.since {
        Some(r) => provider.diff_name_only(repo_dir, &format!("{r}...HEAD"))?,
        None => {
            let mut tracked = provider.diff_name_only(repo_dir, "HEAD")?;
            tracked.extend(provider.untracked_files(repo_dir)?);
            tracked
        }
    };
    Ok(names.into_iter().map(PathBuf::from).collect())
}
```

If `ShellGitProvider` lacks `diff_name_only` / `untracked_files` methods, add them — refer to `crate::git::shell::ShellGitProvider` and follow existing helper patterns (look for `diff_hunks` etc.). Smallest possible additions.

- [ ] **Step 4: Run — passes**

- [ ] **Step 5: Commit**

### Task 2.4: aggregate.rs — per-file constituent dispatch + filter

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/review/aggregate.rs`

Constituents to call (MVP includes all 5 per user decision):

| Constituent | Library call | Per-file filter |
|---|---|---|
| `impact` | `impact::build_payload` with `baseline = since` | `risk_level >= medium` only |
| `coverage` | `coverage::build_payload` | only BlindSpot rows for files in scope |
| `egress` (was `tool_map`) | `tool_map::build_payload` + diff (added-only via git hunks) | only files where new external-call call-sites appear vs baseline |
| `shape_check` | `shape_check::build_payload` | only routes touched by changed files |
| `diff` (resolver) | `diff::build_payload` with `--section bindings` | binding tier-degradation rows only |

- [ ] **Step 1: Failing test — empty file list yields empty report**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    #[test]
    fn empty_file_list_yields_empty_report() {
        let engine = crate::tests::test_helpers::load_fixture_engine("simple_python");
        let r = run(&[], &PathBuf::from("."), &engine).unwrap();
        assert!(r.findings.is_empty());
        assert_eq!(r.files_reviewed, 0);
    }
}
```

- [ ] **Step 2: Run — fails (run undefined)**

- [ ] **Step 3: Implement `pub fn run(files: &[PathBuf], repo_dir: &Path, engine: &Engine) -> Result<Report, GnxError>`**

Skeleton:
```rust
use super::findings::{Finding, Report, Severity, Source};
use crate::commands::{coverage, diff, impact, shape_check, tool_map};
use crate::engine::Engine;
use graph_nexus_core::GnxError;
use std::path::{Path, PathBuf};

pub fn run(files: &[PathBuf], repo_dir: &Path, engine: &Engine) -> Result<Report, GnxError> {
    let mut findings = Vec::new();

    let impact_payload = build_impact(files, engine)?;
    findings.extend(map_impact(&impact_payload)?);

    let coverage_payload = build_coverage(files, repo_dir)?;
    findings.extend(map_coverage_blind_spots(&coverage_payload, files));

    let egress = build_egress_diff(files, repo_dir, engine)?;
    findings.extend(egress);

    let shape_check_payload = build_shape_check(files, engine)?;
    findings.extend(map_shape_check(&shape_check_payload, files));

    let resolver = build_resolver_diff(files, repo_dir, engine)?;
    findings.extend(resolver);

    Ok(Report { findings, files_reviewed: files.len() })
}

// Per-constituent helpers (one per row in the table above) — each
// constructs the constituent's Args struct scoped to `files`, calls
// build_payload, then walks the resulting Value to extract Findings.
fn build_impact(files: &[PathBuf], engine: &Engine) -> Result<serde_json::Value, GnxError> { /* ... */ todo!() }
fn map_impact(v: &serde_json::Value) -> Result<Vec<Finding>, GnxError> { /* filter risk_level >= medium */ todo!() }
// ... etc
```

Each helper requires implementation. Write tests for each helper individually before implementing — the same red→green→commit cycle.

- [ ] **Step 4: Sub-task per constituent — TDD each helper**

  For each of (impact, coverage, egress, shape_check, resolver):
  - [ ] Write a unit test against a known-output fixture payload
  - [ ] Run — fails
  - [ ] Implement the helper
  - [ ] Run — passes
  - [ ] Commit

The `egress` helper specifically needs the "new vs base" filter: load the per-file added lines from `git diff <since>...HEAD -- <file>` hunks (use `crate::git::parse_diff_hunks`), then check each `tool_map` call-site row against the added-line set. Only call-sites within added lines become findings.

- [ ] **Step 5: Final integration test**

End-to-end test: fixture repo with seeded change → `aggregate::run` → assert specific findings appear with expected severities/lines.

```bash
cargo test -p graph-nexus --lib commands::review::aggregate
```

- [ ] **Step 6: Commit incrementally — one commit per constituent helper.**

### Task 2.5: Wire `gnx review` into main.rs

**Files:**
- Modify: `crates/graph-nexus-cli/src/main.rs`

- [ ] **Step 1: Add CLI variant**

In `enum Commands` after `Routes`:

```rust
/// LLM-workflow audit aggregator — runs impact, coverage(blind-spot),
/// egress(diff), shape-check, and resolver-diff over changed files in
/// one shot, filtered to high-confidence signals only.
Review(commands::review::ReviewArgs),
```

- [ ] **Step 2: Add repo dispatch arm**

```rust
Commands::Review(args) => args.repo.as_deref(),
```

- [ ] **Step 3: Add run dispatch arm**

```rust
Commands::Review(args) => commands::review::run(args, &engine),
```

- [ ] **Step 4: Build + smoke test**

```bash
cargo build -p graph-nexus --bin gnx --release
./target/release/gnx review --help
./target/release/gnx review --files crates/graph-nexus-cli/src/main.rs --format json | head -20
```

Expected: `--help` lists the review command; smoke run produces a JSON payload (may be empty `status: clean` if no findings).

- [ ] **Step 5: Commit**

### Task 2.6: cli_surface test update + integration test

**Files:**
- Modify: `crates/graph-nexus-cli/tests/cli_surface.rs`
- Create: `crates/graph-nexus-cli/tests/review_e2e.rs`

- [ ] **Step 1: Update cli_surface to assert review appears**

Add `"review"` to the agent-commands list.

- [ ] **Step 2: End-to-end test — fixture repo, real `gnx review` invocation, assert spec-compliant output**

```rust
// crates/graph-nexus-cli/tests/review_e2e.rs
#[test]
fn review_no_findings_emits_clean_status() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["review", "--files", "Cargo.toml", "--format", "json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["status"], "clean", "out: {}", String::from_utf8_lossy(&out.stdout));
}
```

(Add a happy-path "findings present" test against a fixture with intentional issues.)

- [ ] **Step 3: Run**

```bash
cargo test -p graph-nexus --test review_e2e
cargo test -p graph-nexus --test cli_surface
```

- [ ] **Step 4: Commit**

---

## Phase 3 — Remove standalone CLI for 5 constituents + rename tool_map → egress

After Phase 2 ships and stabilizes, remove the now-redundant top-level subcommands.

### Task 3.1: Remove the 5 standalone CLI variants

**Files:**
- Modify: `crates/graph-nexus-cli/src/main.rs`

- [ ] **Step 1: Update failing tests first**

Modify `cli_surface.rs::top_level_lists_nine_agent_commands` to its post-refactor name & list — strip `impact`, `coverage`, `tool-map`, `shape-check`, `diff` from the list. Add positive assertions: these commands must NOT appear.

```rust
#[test]
fn top_level_does_not_list_removed_commands() {
    let help = gnx_help();
    for cmd in ["impact", "coverage", "tool-map", "shape-check", "diff"] {
        for line in help.lines() {
            let t = line.trim_start();
            if t.starts_with(&format!("{cmd} ")) || t.starts_with(&format!("{cmd}\t")) {
                panic!("{cmd} still in --help: {line}");
            }
        }
    }
}
```

- [ ] **Step 2: Run — fails (commands still exist)**

- [ ] **Step 3: Remove `Impact`, `Coverage`, `ShapeCheck`, `ToolMap`, `Diff` variants and dispatch arms from main.rs**

Keep `commands::{impact, coverage, shape_check, tool_map, diff}` modules — they're still callable by `review`. Only the CLI variants go.

- [ ] **Step 4: Remove `clap::Args` derives from the Args structs (if you want — purely housekeeping, optional)**

Recommendation: keep the Args structs as-is so review can populate them directly. clap derives don't cost runtime; they only matter if the Args are wired into a `#[derive(Subcommand)]` enum, which they no longer are.

- [ ] **Step 5: Run — all tests pass**

- [ ] **Step 6: Commit**

### Task 3.2: Rename `tool_map` module to `egress`

**Files:**
- Rename: `crates/graph-nexus-cli/src/commands/tool_map.rs` → `egress.rs`
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs` — `pub mod tool_map;` → `pub mod egress;`
- Modify: `crates/graph-nexus-cli/src/commands/review/aggregate.rs` — replace `commands::tool_map` with `commands::egress`
- Modify: any test files referencing `tool_map`

- [ ] **Step 1: `git mv` the file**

```bash
git mv crates/graph-nexus-cli/src/commands/tool_map.rs \
       crates/graph-nexus-cli/src/commands/egress.rs
```

- [ ] **Step 2: Rename module + struct names**

```bash
# In place: rename ToolMapArgs → EgressArgs in the module + at the review aggregate call site.
```

Use `Edit` with `replace_all` on `ToolMapArgs` → `EgressArgs` per file.

- [ ] **Step 3: Run all tests**

- [ ] **Step 4: Commit**

### Task 3.3: Sweep README / docs / skills for the 5 removed command names

- [ ] **Step 1: Find references**

```bash
rg -n 'gnx (impact|coverage|tool-map|shape-check|diff)\b' README*.md docs/ .claude/skills/ 2>/dev/null
```

- [ ] **Step 2: For each hit: either update to mention `gnx review` instead, or remove the line entirely if the original example doesn't have a `gnx review` equivalent.**

- [ ] **Step 3: Commit**

### Task 3.4: PR + release notes

Per `release-note` skill conventions, the Phase 3 PR's release notes should be marked `BREAKING:` — these subcommands are gone for users.

---

## Self-review checklist

- [x] Spec coverage: `scan` removal (Phase 0), library extraction (Phase 1), aggregator (Phase 2), CLI cleanup + egress rename (Phase 3). `egress` diff-aware filter explicitly called out in Task 2.4.
- [x] No placeholders: each TDD cycle has concrete commands and expected outputs. The aggregate helpers (Task 2.4 Step 4) are deferred to per-constituent sub-tasks rather than spelled out — acceptable because each helper is small and self-similar; the strategy ("filter Value rows by criterion, map to Finding") is concrete.
- [x] Type consistency: `Finding` / `Report` / `Severity` / `Source` defined once in findings.rs; consumers in scope.rs / aggregate.rs / mod.rs use the same names.
- [x] Removed scan completely (source, tests, docs, identifier_finder, MCP via auto-derive).
- [x] MCP schema auto-derives from clap — no manual MCP changes needed for Phase 0/3.

## Open questions for the executor

1. **MVP shape_check + resolver diff feasibility:** Spec hints these need cross-file context the MVP doesn't yet have. If implementation reveals this is true, Task 2.4 sub-tasks for shape_check and resolver should emit `BlindSpot`-style "needs follow-up" findings rather than wrong data, and a follow-up issue logged.
2. **Egress baseline computation:** "new external calls vs base" requires running `tool_map::build_payload` against the baseline tree too. Possible approaches: (a) git-checkout the baseline into a temp dir, run analyzer fresh — slow; (b) maintain a per-commit tool_map cache. Recommend (a) for MVP, profile, decide on (b) later.
3. **Identifier_finder POC deletion:** Phase 0 Task 0.6 picks "delete per CLAUDE.md no-dead-code rule"; user can override to "keep per spec" with a single-line edit.
