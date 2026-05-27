# Dump-Resolver CLI Re-wire Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ecp diff --section bindings` (and `all`) work again by threading the `--dump-resolver` path from the `admin index` CLI into the already-complete analyzer dump path.

**Architecture:** The analyzer (`GraphBuilder.with_resolver_dump` → `write_resolver_dump`) already works and is tested. Only CLI wiring is missing. Approach A: when `--dump-resolver` is set, `admin index` bypasses `build_l2` (whose same-SHA fast-path attach would skip the analyzer) and calls `run_analyzer_for_paths` directly with the dump path, discarding the graph and keeping only the JSONL dump.

**Tech Stack:** Rust, clap, the ecp-cli + ecp-analyzer crates. Tests: `cargo test -p egent-code-plexus`.

**Spec:** `docs/superpowers/specs/2026-05-27-dump-resolver-cli-rewire-design.md`

---

## File Structure

- `crates/ecp-cli/src/commands/admin/index.rs` — add `dump_resolver` param to `run_analyzer_for_paths`; add bypass branch in `run()`.
- `crates/ecp-cli/src/build/orchestrator.rs:150` — pass `None` for the new param (normal build never dumps).
- `crates/ecp-cli/tests/diff_bindings_test.rs:28` — un-ignore the integration test.
- `crates/ecp-cli/src/commands/review/mod.rs:73` — re-add `DiffSection::Bindings` to verdicts.

---

### Task 1: Thread `dump_resolver` into `run_analyzer_for_paths`

**Files:**
- Modify: `crates/ecp-cli/src/commands/admin/index.rs:69` (signature) and `:327` (GraphBuilder construction)
- Modify: `crates/ecp-cli/src/build/orchestrator.rs:150` (caller passes `None`)

This task is a pure signature plumbing change. It does not change runtime behavior
yet (the only caller passes `None`), so the existing build path is unaffected. We
verify with a compile + existing-test run.

- [ ] **Step 1: Add the parameter to the signature**

In `crates/ecp-cli/src/commands/admin/index.rs`, change the signature at line 69:

```rust
pub fn run_analyzer_for_paths(
    src_root: &std::path::Path,
    out_dir: &std::path::Path,
    parse_cache_root: Option<&std::path::Path>,
    dump_resolver: Option<&std::path::Path>,
) -> std::io::Result<(usize, ecp_core::graph::ZeroCopyGraph)> {
```

- [ ] **Step 2: Wire it into the GraphBuilder**

In the same function, at the `GraphBuilder::new()` site (currently line 327):

```rust
    let mut builder = GraphBuilder::new()
        .with_path_aliases(aliases)
        .with_repo_root(src_root.to_path_buf())
        .with_resolver_dump(dump_resolver.map(std::path::Path::to_path_buf));
```

(`with_resolver_dump(None)` is a no-op — the field defaults to `None`.)

- [ ] **Step 3: Update the orchestrator caller**

In `crates/ecp-cli/src/build/orchestrator.rs`, the call at line 150 becomes:

```rust
        let (node_count, global_graph) = crate::commands::admin::index::run_analyzer_for_paths(
            &src_root,
            building,
            Some(repo_root),
            None,
        )?;
```

- [ ] **Step 4: Compile**

Run: `cargo build -p egent-code-plexus 2>&1 | tail -20`
Expected: builds clean (no other callers exist — verified by `rg run_analyzer_for_paths crates/ --type rust`).

- [ ] **Step 5: Run existing index/build tests to confirm no regression**

Run: `cargo test -p egent-code-plexus --test build_orchestrator 2>&1 | tail -15`
Expected: PASS (or the existing `#[ignore]` cases stay ignored; no new failures).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/commands/admin/index.rs crates/ecp-cli/src/build/orchestrator.rs
git commit -m "refactor(index): thread dump_resolver param into run_analyzer_for_paths"
```

---

### Task 2: Bypass branch in `admin index::run` when dumping

**Files:**
- Modify: `crates/ecp-cli/src/commands/admin/index.rs:371-377` (replace the warning-only block)

The current block (lines 372-377) only prints a warning. Replace it with a real
bypass: build the graph via `run_analyzer_for_paths` into a scratch temp dir
(graph discarded), writing the dump JSONL to the user-specified path, then return
early — never touching `build_l2`.

- [ ] **Step 1: Write the failing test for the wiring**

Create `crates/ecp-cli/tests/dump_resolver_wiring_test.rs`:

```rust
//! `ecp admin index --dump-resolver <out>` writes a non-empty resolver JSONL
//! whose lines deserialize as binding decisions. Pins the CLI wiring (the
//! GraphBuilder-level round-trip is covered in builder.rs unit tests).

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn admin_index_dump_resolver_writes_jsonl() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(
        repo.join("src/a.ts"),
        "import { helper } from \"./b\";\nexport function main() { helper(); }\n",
    )
    .expect("write a.ts");
    std::fs::write(repo.join("src/b.ts"), "export function helper() { return 1; }\n")
        .expect("write b.ts");

    // git repo required: head_sha_hex is read before the bypass branch.
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
    ] {
        assert!(Command::new("git").args(&args).current_dir(repo).output().unwrap().status.success());
    }
    assert!(Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "v1"])
        .current_dir(repo).output().unwrap().status.success());

    let dump = repo.join("dump.jsonl");
    let out = Command::new(ecp_bin())
        .args([
            "admin", "index",
            "--repo", repo.to_str().unwrap(),
            "--dump-resolver", dump.to_str().unwrap(),
        ])
        .env("HOME", repo)
        .output()
        .expect("run admin index --dump-resolver");

    assert!(
        out.status.success(),
        "admin index --dump-resolver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(&dump)
        .unwrap_or_else(|e| panic!("dump file not written: {e}"));
    assert!(!body.trim().is_empty(), "dump JSONL is empty");
    // Every line must be a JSON object with at least src_file + name.
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("bad JSONL line {line:?}: {e}"));
        assert!(v.get("src_file").is_some(), "missing src_file: {line}");
        assert!(v.get("name").is_some(), "missing name: {line}");
    }
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p egent-code-plexus --test dump_resolver_wiring_test 2>&1 | tail -20`
Expected: FAIL — `dump file not written` (the current warning-only block never writes it).

- [ ] **Step 3: Implement the bypass branch**

In `crates/ecp-cli/src/commands/admin/index.rs`, replace the block at lines 372-377
(the `if args.dump_resolver.is_some() { eprintln!("warning: ...") }`) with:

```rust
    // --dump-resolver: produce a resolver-decision JSONL side-output. This is
    // a debug / diff path (consumed by `ecp diff --section bindings` and the
    // oracle harness), NOT a publish. Bypass build_l2 — its same-SHA fast-path
    // attach would skip the analyzer, so the dump would never be produced — and
    // discard the graph; only the JSONL is wanted. ~/.ecp is left untouched.
    if let Some(dump_path) = args.dump_resolver.clone() {
        let worktree = std::path::PathBuf::from(&args.repo);
        if !worktree.exists() {
            return Err(format!("repo path does not exist: {}", worktree.display()));
        }
        let scratch = std::env::temp_dir().join(format!("ecp-dumponly-{}", std::process::id()));
        std::fs::create_dir_all(&scratch)
            .map_err(|e| format!("create scratch dir: {e}"))?;
        let result = run_analyzer_for_paths(&worktree, &scratch, None, Some(&dump_path))
            .map_err(|e| format!("analyzer dump pass: {e}"));
        let _ = std::fs::remove_dir_all(&scratch);
        result?;
        return Ok(());
    }
```

- [ ] **Step 4: Run the wiring test to confirm it passes**

Run: `cargo test -p egent-code-plexus --test dump_resolver_wiring_test 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Confirm normal index still bypasses the dump path**

Run: `cargo test -p egent-code-plexus --test build_orchestrator 2>&1 | tail -10`
Expected: no new failures (the bypass is `if let Some(...)`-gated, so `dump_resolver = None` runs the original `build_l2` path untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/ecp-cli/src/commands/admin/index.rs crates/ecp-cli/tests/dump_resolver_wiring_test.rs
git commit -m "feat(index): wire --dump-resolver via build_l2 bypass (approach A)"
```

---

### Task 3: Un-ignore the diff bindings integration test

**Files:**
- Modify: `crates/ecp-cli/tests/diff_bindings_test.rs:28` (remove `#[ignore]`)

The test `diff_bindings_two_commit_resolution_change` is already written in v2
terms and exercises the full `diff --section bindings` path. With Tasks 1-2 landed,
`diff`'s subprocess `ecp admin index --dump-resolver` now writes the JSONL, so the
test should pass.

- [ ] **Step 1: Remove the ignore attribute**

In `crates/ecp-cli/tests/diff_bindings_test.rs`, delete line 28 entirely:

```rust
#[ignore = "requires --dump-resolver which is deferred in v2 (build_l2 doesn't yet wire it); restore after Phase 5+ overlay merge lands and --dump-resolver is re-implemented"]
```

So the test becomes:

```rust
#[test]
fn diff_bindings_two_commit_resolution_change() {
```

- [ ] **Step 2: Run it to confirm it passes**

Run: `cargo test -p egent-code-plexus --test diff_bindings_test 2>&1 | tail -20`
Expected: PASS for both `diff_bindings_two_commit_resolution_change` and
`diff_bindings_against_head_yields_empty`.

If it fails on the assertion `total_changes > 0`, inspect the actual JSON output
(the test panics with the `bindings` payload) — a real bindings-diff content issue,
not wiring; debug before proceeding.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/tests/diff_bindings_test.rs
git commit -m "test(diff): un-ignore bindings two-commit test now that dump-resolver is wired"
```

---

### Task 4: Re-enable bindings in `review` verdicts

**Files:**
- Modify: `crates/ecp-cli/src/commands/review/mod.rs:73-93` (add `DiffSection::Bindings`, drop the stale comment)

`run_verdicts` deliberately skipped bindings because dump-resolver was deferred.
Now that it works, add bindings back so verdicts include tier-degradation /
target-change signal.

- [ ] **Step 1: Add `DiffSection::Bindings` and remove the stale comment**

In `crates/ecp-cli/src/commands/review/mod.rs`, replace the comment + `section` vec
(lines ~78-84) so the comment no longer claims bindings is deferred and the vec
includes `Bindings`:

```rust
    let diff_args = DiffArgs {
        section: vec![
            DiffSection::Bindings,
            DiffSection::Routes,
            DiffSection::Contracts,
            DiffSection::Symbols,
        ],
        baseline: Some(since.to_string()),
        baseline_graph: None,
        current_graph: None,
        format: None,
        verbose: false,
        repo: args.repo.clone(),
    };
```

Delete the three comment lines immediately above `let diff_args` that start with
`// Skip bindings: --dump-resolver path is deferred in v2 ...`.

- [ ] **Step 2: Compile**

Run: `cargo build -p egent-code-plexus 2>&1 | tail -10`
Expected: builds clean.

- [ ] **Step 3: Verify review still runs end-to-end**

Run: `cargo test -p egent-code-plexus review 2>&1 | tail -15`
Expected: no new failures. (If no review-specific test exists, this is a no-op
compile gate; the manual check in Task 5 covers behavior.)

- [ ] **Step 4: Commit**

```bash
git add crates/ecp-cli/src/commands/review/mod.rs
git commit -m "feat(review): re-enable bindings in verdicts now that dump-resolver works"
```

---

### Task 5: Manual end-to-end verification + full test suite

**Files:** none (verification only)

- [ ] **Step 1: Build the release binary used by the manual check**

Run: `cargo build -p egent-code-plexus 2>&1 | tail -5`
Expected: clean build. Note the debug binary path is `target/debug/ecp`.

- [ ] **Step 2: Reproduce the original crash is now fixed**

Run (from this worktree, which is a git repo):
```bash
./target/debug/ecp diff --section bindings --baseline HEAD~3 2>&1 | tail -10; echo "EXIT: ${PIPESTATUS[0]}"
```
Expected: EXIT 0, structured bindings output (was EXIT 1 `output encode error: read /tmp/...jsonl: No such file`).

- [ ] **Step 3: Verify `--section all` no longer crashes**

Run:
```bash
./target/debug/ecp diff --section all --baseline HEAD~3 2>&1 | tail -15; echo "EXIT: ${PIPESTATUS[0]}"
```
Expected: EXIT 0, all four sections (bindings, routes, contracts, symbols) rendered.

- [ ] **Step 4: Confirm `~/.ecp` was not polluted by a redundant graph generation**

Run:
```bash
ls -1 ~/.ecp/telemetry/ >/dev/null 2>&1 || true
# scratch dirs must be cleaned up:
ls -d /tmp/ecp-dumponly-* 2>/dev/null && echo "LEAK: scratch dir survived" || echo "OK: no scratch leak"
```
Expected: `OK: no scratch leak`.

- [ ] **Step 5: Full crate test suite**

Run: `cargo test -p egent-code-plexus 2>&1 | tail -25`
Expected: PASS (no `--dump-resolver`-related ignores remain; no new failures).

- [ ] **Step 6: Commit any incidental fixes; otherwise nothing to commit**

If steps surfaced a fix, commit it with a descriptive message. Otherwise this task
produces no commit.

---

## Notes for the implementer

- This worktree's session CWD may reset between shell calls (harness quirk). Always
  run commands with the worktree as CWD: `cd /home/enor/code-graph-nexus/.claude/worktrees/feat-dump-resolver-rewire && <cmd>`.
- **No `Co-Authored-By` trailer and no "Generated with Claude Code" footer** in any
  commit message or PR body.
- Before pushing: run `/simplify` (or, since this is a small diff, a single-pass
  self-review) per project convention. Then open a PR from `feat/dump-resolver-rewire`
  via `gh pr create` — do not push the branch to `main` directly.
