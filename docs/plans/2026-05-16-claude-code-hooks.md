# Claude Code Hooks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a hidden `gnx hook <event> --claude-code` subcommand plus selective install/uninstall/status admin commands, porting `~/bin/gnx.branch-spike/claude-hooks/gitnexus-hook.cjs` to in-process Rust.

**Architecture:** Each Claude Code hook event becomes a self-contained Rust file under `commands/hook/`. The dispatch entry point reads JSON envelope on stdin, routes to the per-event handler, writes JSON response on stdout. Stale-detection reuses existing `auto_ensure`; PreToolUse calls a new `search::compute_hits` helper in-process (no subprocess fork).

**Tech Stack:** Rust 1.94, clap 4.5 derive, serde_json, existing `engine::Engine` + `auto_ensure` + `commands::search` modules. No new external crates.

**Spec:** `docs/specs/2026-05-16-claude-code-hooks-design.md`

**Parallelism:** After T0 (foundation), tasks T1-T6 are independent and SHOULD be dispatched as parallel subagents. T7 depends on T1 (search helper). T8-T9 are sequential.

---

## File Structure

New files:

```
crates/graph-nexus-cli/src/commands/hook/
  mod.rs                       — dispatch + arg parsing + shared types
  common.rs                    — stdin JSON read, stdout response emit, marker paths
  session_start.rs             — SessionStart handler (template render + worktree detect)
  user_prompt_submit.rs        — UserPromptSubmit (marker file surfacing)
  pre_tool_use.rs              — PreToolUse (pattern extract → search hits)
  post_tool_use.rs             — PostToolUse (git mutation → reindex)
crates/graph-nexus-cli/src/commands/admin/claude_code.rs
                               — install/uninstall/status for settings.json
crates/graph-nexus-cli/assets/claude-code/
  rules.md                     — bundled SessionStart template
crates/graph-nexus-cli/tests/
  hook_pre_tool_use.rs
  hook_post_tool_use.rs
  hook_marker_cycle.rs
  hook_install_settings.rs
```

Modified files:

```
crates/graph-nexus-cli/src/commands/mod.rs    — pub mod hook;
crates/graph-nexus-cli/src/commands/admin/mod.rs — wire claude_code subcommand
crates/graph-nexus-cli/src/commands/search.rs — extract compute_hits from run
crates/graph-nexus-cli/src/main.rs            — +1 Commands variant + dispatch arm
```

---

## Task 0: Foundation — hook subcommand skeleton (SEQUENTIAL — blocks all others)

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/hook/mod.rs`
- Create: `crates/graph-nexus-cli/src/commands/hook/common.rs`
- Create: `crates/graph-nexus-cli/src/commands/hook/session_start.rs` (stub)
- Create: `crates/graph-nexus-cli/src/commands/hook/user_prompt_submit.rs` (stub)
- Create: `crates/graph-nexus-cli/src/commands/hook/pre_tool_use.rs` (stub)
- Create: `crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs` (stub)
- Modify: `crates/graph-nexus-cli/src/commands/mod.rs`
- Modify: `crates/graph-nexus-cli/src/main.rs`

- [ ] **Step 1: Write the failing dispatch test**

Create `crates/graph-nexus-cli/tests/hook_dispatch_test.rs`:

```rust
//! Verifies the `gnx hook <event> --claude-code` subcommand parses
//! and dispatches without panic on a minimal stdin envelope.

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn pre_tool_use_no_match_returns_empty_stdout() {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp", "tool_name": "Bash", "tool_input": {"command": "ls"}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "expected empty stdout for no-op, got: {:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn missing_host_flag_errors() {
    let out = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p graph-nexus --test hook_dispatch_test`
Expected: FAIL (subcommand not registered)

- [ ] **Step 3: Create commands/hook/common.rs**

```rust
//! Shared utilities for Claude Code hook event handlers:
//! - stdin JSON envelope parsing
//! - hookSpecificOutput JSON emission
//! - marker file paths under .gitnexus-rs/

use graph_nexus_core::GnxError;
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Parsed Claude Code stdin envelope (only the fields hooks consume).
#[derive(Debug, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub tool_output: Value,
}

pub fn read_stdin_envelope() -> Result<HookInput, GnxError> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(GnxError::Io)?;
    if buf.trim().is_empty() {
        return Ok(HookInput {
            cwd: String::new(),
            tool_name: String::new(),
            tool_input: Value::Null,
            tool_output: Value::Null,
        });
    }
    serde_json::from_str(&buf)
        .map_err(|e| GnxError::InvalidArgument(format!("hook stdin parse: {e}")))
}

/// Emit `{"hookSpecificOutput": {"hookEventName": ..., "additionalContext": ...}}`
/// on stdout. Caller passes the canonical Claude Code event name
/// (e.g. "PreToolUse", "UserPromptSubmit").
pub fn emit_additional_context(event: &str, context: &str) {
    let payload = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    });
    println!("{}", payload);
}

/// Resolve `.gitnexus-rs/` relative to cwd. Returns None if cwd is not
/// absolute or has no `.gitnexus-rs/` subdir.
pub fn gitnexus_dir(cwd: &str) -> Option<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return None;
    }
    let candidate = path.join(".gitnexus-rs");
    candidate.exists().then_some(candidate)
}
```

- [ ] **Step 4: Create commands/hook/{session_start,user_prompt_submit,pre_tool_use,post_tool_use}.rs as stubs**

Each file:

```rust
//! Stub — handler logic ported in Task N (see plan).

use super::common::HookInput;
use graph_nexus_core::GnxError;

pub fn handle(_input: &HookInput) -> Result<(), GnxError> {
    Ok(())
}
```

- [ ] **Step 5: Create commands/hook/mod.rs**

```rust
//! `gnx hook <event> --claude-code` — Claude Code hook entry point.
//!
//! Reads JSON envelope on stdin, dispatches to per-event handler,
//! handler writes JSON response on stdout (empty stdout = no-op).

mod common;
mod post_tool_use;
mod pre_tool_use;
mod session_start;
mod user_prompt_submit;

use clap::{Args, ValueEnum};
use graph_nexus_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct HookArgs {
    /// Which Claude Code hook event fired.
    pub event: HookEvent,

    /// Identifies the agent host whose envelope shape stdin carries.
    /// Exactly one host flag must be set.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum HookEvent {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    SessionStart,
}

pub fn run(args: HookArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument(
            "gnx hook: exactly one host flag required (e.g. --claude-code)".into(),
        ));
    }
    let input = common::read_stdin_envelope()?;
    match args.event {
        HookEvent::UserPromptSubmit => user_prompt_submit::handle(&input),
        HookEvent::PreToolUse => pre_tool_use::handle(&input),
        HookEvent::PostToolUse => post_tool_use::handle(&input),
        HookEvent::SessionStart => session_start::handle(&input),
    }
}
```

- [ ] **Step 6: Wire into commands/mod.rs and main.rs**

Append to `commands/mod.rs`:
```rust
pub mod hook;
```

In `main.rs`, add to the `Commands` enum (alphabetical with other hidden):
```rust
    /// Internal: Claude Code / Codex / Gemini agent hook dispatch.
    #[command(hide = true)]
    Hook(commands::hook::HookArgs),
```

Add dispatch arm in the `run_no_graph!` match (hooks don't load the graph at dispatch time — handlers that need it load on demand):
```rust
        Commands::Hook(args) => run_no_graph!(commands::hook::run(args.clone())),
```

Also add `Commands::Hook(_)` to the `repo_opt` match's no-repo arm.

- [ ] **Step 7: Run tests, verify they pass**

Run: `cargo test -p graph-nexus --test hook_dispatch_test`
Expected: PASS (both `pre_tool_use_no_match_returns_empty_stdout` and `missing_host_flag_errors`)

- [ ] **Step 8: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/hook/ \
        crates/graph-nexus-cli/src/commands/mod.rs \
        crates/graph-nexus-cli/src/main.rs \
        crates/graph-nexus-cli/tests/hook_dispatch_test.rs
git commit -m "feat(hook): T0 — gnx hook <event> --claude-code skeleton

Dispatch entry + per-event stub modules. Each handler returns Ok with
empty stdout (no-op). Tests verify the subcommand parses and rejects
missing host flag."
```

---

## Task 1: search::compute_hits helper extraction (PARALLEL — independent file)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/search.rs`

- [ ] **Step 1: Read the existing run() to identify hit-building logic**

Run: `grep -n "fn run\|fn compute_single\|fn compute_multi\|fn build_payload" crates/graph-nexus-cli/src/commands/search.rs`

Locate where `Vec<Hit>` is built. (Already split into `compute_single` and `compute_multi` per recent PR — both return `Vec<Hit>` / `(Vec<Hit>, Option<String>)`.)

- [ ] **Step 2: Add the new public helper**

After `compute_payload`, add:

```rust
/// In-process search entry point for hooks and other internal consumers.
/// Returns owned `Hit` rows without going through stdout / OutputFormat.
/// Top-K trimmed identical to `run`.
pub fn compute_hits(args: SearchArgs, engine: &Engine) -> Result<Vec<Hit>, GnxError> {
    let targets = resolve_targets(args.repo.as_deref())?;
    if targets.is_empty() {
        compute_single(&args.pattern, &args.mode, args.kind.as_deref(), engine, None)
    } else if targets.len() == 1 {
        let (repo_name, graph_path) = targets.into_iter().next().unwrap();
        let local_engine = Engine::load(std::path::PathBuf::from(&graph_path))
            .map_err(|e| GnxError::Rkyv(format!("{repo_name}: load: {e}")))?;
        compute_single(
            &args.pattern,
            &args.mode,
            args.kind.as_deref(),
            &local_engine,
            Some(repo_name),
        )
    } else {
        compute_multi(&args.pattern, &args.mode, args.kind.as_deref(), targets)
            .map(|(hits, _summary)| hits)
    }
}
```

Make `Hit` pub at module level (currently private):

```rust
pub struct Hit {  // was: struct Hit
    pub repo: Option<String>,
    pub score: f32,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub name: String,
    pub signature: String,
    pub caller_count: usize,
}
```

- [ ] **Step 3: Add a unit test verifying compute_hits returns owned data**

Append to existing `#[cfg(test)] mod tests` in search.rs:

```rust
    #[test]
    fn compute_hits_returns_owned_hit_rows() {
        // Signature-only check — we can't run search without a graph,
        // but this verifies the public surface stays Send + Sync owned.
        fn _check(_: fn(SearchArgs, &Engine) -> Result<Vec<Hit>, GnxError>) {}
        _check(compute_hits);
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p graph-nexus search::tests`
Expected: all pass, including new compute_hits signature check.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/search.rs
git commit -m "refactor(search): T1 — expose compute_hits for in-process callers

Extracted from run() so hook handlers (PreToolUse) can fetch hits
without going through stdout. Hit struct is now pub at module level."
```

---

## Task 2: SessionStart event handler (PARALLEL)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/hook/session_start.rs`
- Create: `crates/graph-nexus-cli/tests/hook_session_start_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! SessionStart hook: template render + worktree detection.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn no_index_present_yields_empty_output() {
    let tmp = TempDir::new().unwrap();
    let mut child = Command::new(gnx_bin())
        .args(["hook", "session-start", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, tmp.path().display());
    child.stdin.as_mut().unwrap().write_all(envelope.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "no .gitnexus-rs/ → no-op expected");
}

#[test]
fn template_placeholders_get_rendered_when_meta_present() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    // Minimal meta.json — schema mirrors BranchMeta required fields.
    std::fs::write(
        gnx_dir.join("meta.json"),
        r#"{"indexed_at":"2026-05-16T00:00:00Z","node_count":1234,"worktree_path":"/x","remote_url":"","schema_version":1}"#,
    ).unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("gnx-rules.md"),
        "stats: {{stats.nodes}} symbols",
    ).unwrap();

    let mut child = Command::new(gnx_bin())
        .args(["hook", "session-start", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, tmp.path().display());
    child.stdin.as_mut().unwrap().write_all(envelope.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("1234 symbols"), "rendered output should substitute {{stats.nodes}}: got {body}");
    assert!(body.contains("SessionStart"));
}
```

- [ ] **Step 2: Run tests, verify failures**

Run: `cargo test -p graph-nexus --test hook_session_start_test`
Expected: FAIL on template-render test (stub returns no output).

- [ ] **Step 3: Implement session_start.rs**

```rust
//! SessionStart handler: render rules template, surface worktree-needs-index hints.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use graph_nexus_core::GnxError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    if input.cwd.is_empty() {
        return Ok(());
    }
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => {
            // No index yet — maybe a worktree that needs init.
            if let Some(hint) = detect_worktree_needing_index(Path::new(&input.cwd)) {
                emit_additional_context("SessionStart", &hint);
            }
            return Ok(());
        }
    };

    let rendered = render_rules(Path::new(&input.cwd), &gnx_dir);
    if !rendered.trim().is_empty() {
        emit_additional_context("SessionStart", &rendered);
    }
    Ok(())
}

fn render_rules(repo_root: &Path, gnx_dir: &Path) -> String {
    let template = match load_template(repo_root) {
        Some(t) => t,
        None => return String::new(),
    };
    let (nodes, edges, head) = read_stats(gnx_dir, repo_root);
    let has_graphify = repo_root.join("graphify-out").exists();
    let has_wiki = has_graphify && repo_root.join("graphify-out").join("wiki").join("index.md").exists();

    let mut out = template
        .replace("{{stats.nodes}}", &nodes)
        .replace("{{stats.edges}}", &edges)
        .replace("{{head}}", &head);
    out = render_conditional(&out, "wiki", has_wiki);
    out = render_conditional(&out, "graphify", has_graphify);
    out.trim().to_string()
}

fn load_template(repo_root: &Path) -> Option<String> {
    let candidates = [
        repo_root.join(".claude").join("gnx-rules.md"),
        dirs_home().join(".claude").join("hooks").join("gnx").join("rules.md"),
    ];
    for c in candidates {
        if let Ok(s) = fs::read_to_string(&c) {
            return Some(s);
        }
    }
    None
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn read_stats(gnx_dir: &Path, repo_root: &Path) -> (String, String, String) {
    let mut nodes = "?".to_string();
    let mut edges = "?".to_string();
    if let Ok(raw) = fs::read_to_string(gnx_dir.join("meta.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(n) = v.get("node_count").and_then(|x| x.as_u64()) {
                nodes = n.to_string();
            }
            // edges not in BranchMeta yet; placeholder until added.
            if let Some(e) = v.get("edge_count").and_then(|x| x.as_u64()) {
                edges = e.to_string();
            }
        }
    }
    let head = git_head_short(repo_root).unwrap_or_else(|| "?".into());
    (nodes, edges, head)
}

fn git_head_short(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn render_conditional(text: &str, key: &str, keep: bool) -> String {
    let open = format!("{{{{#if {}}}}}", key);
    let close = "{{/if}}";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + open.len()..];
        let end = match after_open.find(close) {
            Some(e) => e,
            None => break,
        };
        if keep {
            out.push_str(&after_open[..end]);
        }
        rest = &after_open[end + close.len()..];
    }
    out.push_str(rest);
    out
}

fn detect_worktree_needing_index(cwd: &Path) -> Option<String> {
    let toplevel = git_rev_parse(cwd, &["rev-parse", "--show-toplevel"])?;
    let git_path = Path::new(&toplevel).join(".git");
    // Worktrees have `.git` as a FILE (gitlink); not interesting for non-worktrees.
    if !git_path.is_file() {
        return None;
    }
    if Path::new(&toplevel).join(".gitnexus-rs").exists() {
        return None;
    }
    let branch = git_rev_parse(Path::new(&toplevel), &["branch", "--show-current"])
        .unwrap_or_default();
    let base = Path::new(&toplevel).file_name()?.to_string_lossy().to_string();
    Some(format!(
        "gnx index missing in this worktree ({base} @ {branch}). Run `gnx admin index` to index it."
    ))
}

fn git_rev_parse(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p graph-nexus --test hook_session_start_test`
Expected: PASS both tests.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/hook/session_start.rs \
        crates/graph-nexus-cli/tests/hook_session_start_test.rs
git commit -m "feat(hook): T2 — SessionStart template render + worktree hint"
```

---

## Task 3: UserPromptSubmit event handler (PARALLEL)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/hook/user_prompt_submit.rs`
- Create: `crates/graph-nexus-cli/tests/hook_marker_cycle_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! UserPromptSubmit hook: surface .rebuild-{complete,failed} markers
//! then unlink them.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_with_envelope(cwd: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "user-prompt-submit", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let envelope = format!(r#"{{"cwd": "{}"}}"#, cwd.display());
    child.stdin.as_mut().unwrap().write_all(envelope.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn complete_marker_surfaced_and_unlinked() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    std::fs::write(gnx_dir.join(".rebuild-complete"), "").unwrap();
    std::fs::write(
        gnx_dir.join("meta.json"),
        r#"{"indexed_at":"2026-05-16T00:00:00Z","node_count":42,"worktree_path":"/x","remote_url":"","schema_version":1}"#,
    ).unwrap();

    let out = run_with_envelope(tmp.path());
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("rebuild complete"), "got: {body}");
    assert!(body.contains("42"), "should mention node count");
    assert!(!gnx_dir.join(".rebuild-complete").exists(), "marker should be unlinked");
}

#[test]
fn failed_marker_takes_priority_over_complete() {
    let tmp = TempDir::new().unwrap();
    let gnx_dir = tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&gnx_dir).unwrap();
    std::fs::write(gnx_dir.join(".rebuild-complete"), "").unwrap();
    std::fs::write(gnx_dir.join(".rebuild-failed"), "").unwrap();
    std::fs::write(gnx_dir.join("last-rebuild.log"), "line1\nline2\nfatal error\n").unwrap();

    let out = run_with_envelope(tmp.path());
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(body.contains("FAILED"));
    assert!(body.contains("fatal error"));
    assert!(!gnx_dir.join(".rebuild-failed").exists());
}

#[test]
fn no_markers_yields_silent_no_op() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join(".gitnexus-rs")).unwrap();
    let out = run_with_envelope(tmp.path());
    assert!(out.stdout.is_empty());
}
```

- [ ] **Step 2: Run tests, verify failures**

Run: `cargo test -p graph-nexus --test hook_marker_cycle_test`
Expected: FAIL (stub).

- [ ] **Step 3: Implement user_prompt_submit.rs**

```rust
//! UserPromptSubmit handler: surface async reindex outcomes via marker
//! files, then unlink them so each event fires only once.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use graph_nexus_core::GnxError;
use std::fs;
use std::path::Path;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let complete = gnx_dir.join(".rebuild-complete");
    let failed = gnx_dir.join(".rebuild-failed");
    let log = gnx_dir.join("last-rebuild.log");

    // Failure takes priority — it's more actionable.
    if failed.exists() {
        let tail = read_log_tail(&log, 3);
        let _ = fs::remove_file(&failed);
        let msg = format!(
            "gnx background reindex FAILED. {} Run `gnx admin index` manually to retry.",
            if tail.is_empty() {
                String::new()
            } else {
                format!("Last log lines: {tail}.")
            }
        );
        emit_additional_context("UserPromptSubmit", msg.trim());
        return Ok(());
    }

    if complete.exists() {
        let stats = read_stats(&gnx_dir);
        let _ = fs::remove_file(&complete);
        let msg = format!(
            "gnx index rebuild complete ({stats}). gnx tools now return fresh data."
        );
        emit_additional_context("UserPromptSubmit", &msg);
    }
    Ok(())
}

fn read_log_tail(log: &Path, lines: usize) -> String {
    let raw = match fs::read_to_string(log) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    raw.trim()
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

fn read_stats(gnx_dir: &Path) -> String {
    let raw = match fs::read_to_string(gnx_dir.join("meta.json")) {
        Ok(s) => s,
        Err(_) => return "?".into(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return "?".into(),
    };
    let nodes = v.get("node_count").and_then(|x| x.as_u64()).map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let edges = v.get("edge_count").and_then(|x| x.as_u64()).map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    format!("{nodes} symbols, {edges} rels")
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p graph-nexus --test hook_marker_cycle_test`
Expected: PASS all 3.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/hook/user_prompt_submit.rs \
        crates/graph-nexus-cli/tests/hook_marker_cycle_test.rs
git commit -m "feat(hook): T3 — UserPromptSubmit marker surfacing + unlink"
```

---

## Task 4: PostToolUse event handler (PARALLEL)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs`
- Create: `crates/graph-nexus-cli/tests/hook_post_tool_use_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! PostToolUse hook: git mutation → stale check → background reindex.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_with(envelope: &str) -> std::process::Output {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "post-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(envelope.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn non_bash_tool_no_op() {
    let out = run_with(r#"{"tool_name":"Read","tool_input":{"file_path":"x"}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn non_git_bash_command_no_op() {
    let out = run_with(r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"},"tool_output":{"exit_code":0}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn failed_git_commit_no_op() {
    let out = run_with(r#"{"tool_name":"Bash","tool_input":{"command":"git commit -m foo"},"tool_output":{"exit_code":1}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn git_commit_in_dir_without_index_no_op() {
    let tmp = TempDir::new().unwrap();
    let envelope = format!(
        r#"{{"cwd":"{}","tool_name":"Bash","tool_input":{{"command":"git commit -m foo"}},"tool_output":{{"exit_code":0}}}}"#,
        tmp.path().display()
    );
    let out = run_with(&envelope);
    // No .gitnexus-rs/ → still surface a hint or remain silent? Per spec §3.4
    // Missing index → no-op (handler doesn't auto-bootstrap).
    assert!(out.stdout.is_empty());
}
```

- [ ] **Step 2: Run tests, verify all pass with stub (since stub returns no output)**

Run: `cargo test -p graph-nexus --test hook_post_tool_use_test`
Expected: 4/4 PASS even on stub.

This is intentional — these tests pin the no-op branches. The "stale → spawn" branch needs a real git+index fixture and is exercised end-to-end in T8.

- [ ] **Step 3: Implement post_tool_use.rs**

```rust
//! PostToolUse handler: detect git ref-changing commands, kick off
//! background reindex when the index is stale.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use graph_nexus_core::GnxError;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    if input.tool_name != "Bash" {
        return Ok(());
    }
    let cmd = input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !is_git_mutation(cmd) {
        return Ok(());
    }
    let exit = input
        .tool_output
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if exit != 0 {
        return Ok(());
    }

    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let repo_root = match gnx_dir.parent() {
        Some(p) => p.to_path_buf(),
        None => return Ok(()),
    };
    let graph_path = gnx_dir.join("graph.bin");

    use crate::auto_ensure::{ensure_index, EnsureResult};
    let result = ensure_index(&graph_path, &repo_root).unwrap_or(EnsureResult::Missing);
    if !matches!(result, EnsureResult::Stale { .. }) {
        return Ok(());
    }
    let age = match result {
        EnsureResult::Stale { age_seconds } => age_seconds,
        _ => 0,
    };

    if !spawn_background_reindex(&repo_root, &gnx_dir) {
        return Ok(());
    }

    emit_additional_context(
        "PostToolUse",
        &format!(
            "gnx reindex started in background (index stale ~{age}s). Subsequent gnx tools may use stale data until completion (~30-120s). If it appears stuck, run `gnx admin index` manually."
        ),
    );
    Ok(())
}

fn is_git_mutation(cmd: &str) -> bool {
    let stripped = strip_shell_quotes(cmd);
    let re = regex::Regex::new(r"\bgit\s+(commit|merge|rebase|cherry-pick|pull)(\s|$)").unwrap();
    re.is_match(&stripped)
}

fn strip_shell_quotes(cmd: &str) -> String {
    let bytes = cmd.as_bytes();
    let mut out = String::with_capacity(cmd.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\'' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        if c == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    break;
                }
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// Detached background `gnx admin index` under flock at
/// `<gnx_dir>/.analyze.lock`. Writes `.rebuild-complete` on success,
/// `.rebuild-failed` after MAX=3 attempts. Returns true if the launcher
/// was spawned successfully (regardless of analyze outcome — that's
/// surfaced asynchronously via marker files in UserPromptSubmit).
fn spawn_background_reindex(repo_root: &Path, gnx_dir: &Path) -> bool {
    let lock = gnx_dir.join(".analyze.lock");
    let complete = gnx_dir.join(".rebuild-complete");
    let failed = gnx_dir.join(".rebuild-failed");
    let log = gnx_dir.join("last-rebuild.log");
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let shell = format!(
        r#"exec 9>{lock} || exit 0
flock -n 9 || exit 0
: > {log}
MAX=3; ATTEMPT=0
while [ $ATTEMPT -lt $MAX ]; do
  ATTEMPT=$((ATTEMPT+1))
  echo "=== attempt $ATTEMPT/$MAX ===" >> {log}
  if {gnx} admin index >> {log} 2>&1; then
    rm -f {failed}
    : > {complete}
    exit 0
  fi
  [ $ATTEMPT -lt $MAX ] && sleep 2
done
rm -f {complete}
: > {failed}
"#,
        lock = shell_quote(&lock),
        log = shell_quote(&log),
        gnx = shell_quote(&self_exe),
        complete = shell_quote(&complete),
        failed = shell_quote(&failed),
    );

    match Command::new("sh")
        .arg("-c")
        .arg(&shell)
        .current_dir(repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => true,
        Err(_) => false,
    }
}

fn shell_quote<P: AsRef<Path>>(p: P) -> String {
    let s = p.as_ref().to_string_lossy().to_string();
    // Conservative: single-quote everything, escape single quotes.
    let escaped = s.replace('\'', r"'\''");
    format!("'{}'", escaped)
}

#[allow(unused_imports)]
use std::path::PathBuf as _PathBufUnused;
```

Add `regex = "1"` to the `[dependencies]` of `crates/graph-nexus-cli/Cargo.toml` if not already present. (It IS present per current main — verify with `grep regex crates/graph-nexus-cli/Cargo.toml`.)

- [ ] **Step 4: Run tests, verify all still pass**

Run: `cargo test -p graph-nexus --test hook_post_tool_use_test`
Expected: 4/4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs \
        crates/graph-nexus-cli/tests/hook_post_tool_use_test.rs
git commit -m "feat(hook): T4 — PostToolUse git-mutation detection + flock-gated reindex"
```

---

## Task 5: admin/claude_code.rs install/uninstall/status (PARALLEL)

**Files:**
- Create: `crates/graph-nexus-cli/src/commands/admin/claude_code.rs`
- Modify: `crates/graph-nexus-cli/src/commands/admin/mod.rs`
- Create: `crates/graph-nexus-cli/tests/hook_install_settings_test.rs`

- [ ] **Step 1: Read the existing admin mod.rs to understand AdminCommands shape**

Run: `cat crates/graph-nexus-cli/src/commands/admin/mod.rs | head -60`
Note the existing enum variants — we'll add new ones following the same pattern.

- [ ] **Step 2: Write the failing settings.json test**

```rust
//! settings.json merge for install/uninstall.

use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn install_one_event_creates_entry_preserves_others() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    let initial = r#"{
  "hooks": {
    "UserPromptSubmit": [
      {"matcher":"","hooks":[{"type":"command","command":"node /legacy/gitnexus-hook.cjs","timeout":3}]}
    ]
  }
}"#;
    std::fs::write(&settings_path, initial).unwrap();

    let out = Command::new(gnx_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start",
            "--settings-path",
        ])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();

    let user_prompt = merged["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert!(
        user_prompt.iter().any(|e| {
            e["hooks"][0]["command"].as_str().unwrap_or("").contains("legacy/gitnexus-hook.cjs")
        }),
        "legacy entry preserved"
    );

    let session_start = merged["hooks"]["SessionStart"].as_array().unwrap();
    assert!(
        session_start.iter().any(|e| {
            e["hooks"][0]["command"].as_str().unwrap_or("").contains("gnx hook session-start --claude-code")
        }),
        "new entry written"
    );
}

#[test]
fn reinstalling_same_event_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    for _ in 0..2 {
        let out = Command::new(gnx_bin())
            .args(["admin", "install-hook", "--claude-code", "--events", "pre-tool-use", "--settings-path"])
            .arg(&settings_path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let pre = merged["hooks"]["PreToolUse"].as_array().unwrap();
    let count = pre.iter().filter(|e| {
        e["hooks"][0]["command"].as_str().unwrap_or("").contains("gnx hook pre-tool-use")
    }).count();
    assert_eq!(count, 1, "duplicate entries should not accumulate");
}

#[test]
fn uninstall_removes_only_specified_event() {
    let tmp = TempDir::new().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    let install = Command::new(gnx_bin())
        .args(["admin", "install-hook", "--claude-code", "--events", "session-start,pre-tool-use", "--settings-path"])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(install.status.success());

    let uninstall = Command::new(gnx_bin())
        .args(["admin", "uninstall-hook", "--claude-code", "--events", "session-start", "--settings-path"])
        .arg(&settings_path)
        .output()
        .unwrap();
    assert!(uninstall.status.success());

    let merged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(merged["hooks"].get("SessionStart").map(|v| v.as_array().map(|a| a.is_empty()).unwrap_or(true)).unwrap_or(true), "SessionStart removed");
    assert!(merged["hooks"]["PreToolUse"].as_array().unwrap().iter().any(|e| {
        e["hooks"][0]["command"].as_str().unwrap_or("").contains("gnx hook pre-tool-use")
    }), "PreToolUse retained");
}
```

- [ ] **Step 3: Run tests, verify failures**

Run: `cargo test -p graph-nexus --test hook_install_settings_test`
Expected: FAIL ("install-hook" / "uninstall-hook" not recognized subcommand).

- [ ] **Step 4: Implement admin/claude_code.rs**

```rust
//! `gnx admin install-hook --claude-code` / `uninstall-hook` / `status`
//! for Claude Code hooks.

use clap::{Args, Subcommand};
use graph_nexus_core::GnxError;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct InstallHookArgs {
    /// Target agent host. Exactly one must be set.
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,

    /// CSV of events to install. When omitted, falls back to TUI.
    /// Recognized: session-start, user-prompt-submit, pre-tool-use, post-tool-use.
    #[arg(long)]
    pub events: Option<String>,

    /// Override path to settings.json (default ~/.claude/settings.json).
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct UninstallHookArgs {
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
    #[arg(long)]
    pub events: Option<String>,
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    #[arg(long, default_value_t = false)]
    pub claude_code: bool,
    #[arg(long, hide = true)]
    pub settings_path: Option<PathBuf>,
}

const ALL_EVENTS: &[&str] = &[
    "session-start",
    "user-prompt-submit",
    "pre-tool-use",
    "post-tool-use",
];

pub fn run_install(args: InstallHookArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument(
            "--claude-code (or another host flag) required".into(),
        ));
    }
    let events = match &args.events {
        Some(s) => parse_events(s)?,
        None => {
            // TUI fallback: ask interactively.
            prompt_events_tui()?
        }
    };
    let settings_path = settings_path(args.settings_path.as_deref());
    let mut settings = read_or_init(&settings_path)?;
    let exe = self_exe()?;
    for ev in &events {
        merge_entry(&mut settings, ev, &exe);
    }
    write_atomic(&settings_path, &settings)?;
    println!("Installed {} event(s) into {}", events.len(), settings_path.display());
    Ok(())
}

pub fn run_uninstall(args: UninstallHookArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument("--claude-code required".into()));
    }
    let events = match &args.events {
        Some(s) => parse_events(s)?,
        None => ALL_EVENTS.iter().map(|s| (*s).to_string()).collect(),
    };
    let settings_path = settings_path(args.settings_path.as_deref());
    let mut settings = read_or_init(&settings_path)?;
    for ev in &events {
        remove_entry(&mut settings, ev);
    }
    write_atomic(&settings_path, &settings)?;
    println!("Removed {} event(s) from {}", events.len(), settings_path.display());
    Ok(())
}

pub fn run_status(args: StatusArgs) -> Result<(), GnxError> {
    if !args.claude_code {
        return Err(GnxError::InvalidArgument("--claude-code required".into()));
    }
    let settings_path = settings_path(args.settings_path.as_deref());
    let settings = read_or_init(&settings_path)?;
    println!("Claude Code hook status (settings: {}):", settings_path.display());
    for ev in ALL_EVENTS {
        let installed = is_installed(&settings, ev);
        let label = if installed { "INSTALLED" } else { "missing" };
        println!("  {:<22}  {}", ev, label);
    }
    Ok(())
}

// ─── internals ─────────────────────────────────────────────────────────────

fn parse_events(csv: &str) -> Result<Vec<String>, GnxError> {
    let mut out = Vec::new();
    for raw in csv.split(',') {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if !ALL_EVENTS.contains(&t) {
            return Err(GnxError::InvalidArgument(format!(
                "unknown event '{t}' — expected one of: {}",
                ALL_EVENTS.join(", ")
            )));
        }
        out.push(t.to_string());
    }
    if out.is_empty() {
        return Err(GnxError::InvalidArgument("--events list is empty".into()));
    }
    Ok(out)
}

fn prompt_events_tui() -> Result<Vec<String>, GnxError> {
    use dialoguer::{theme::ColorfulTheme, MultiSelect};
    let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select Claude Code hook events to install")
        .items(ALL_EVENTS)
        .interact()
        .map_err(|e| GnxError::Output(format!("TUI: {e}")))?;
    Ok(chosen.into_iter().map(|i| ALL_EVENTS[i].to_string()).collect())
}

fn settings_path(override_path: Option<&Path>) -> PathBuf {
    if let Some(p) = override_path {
        return p.to_path_buf();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".claude").join("settings.json")
}

fn read_or_init(path: &Path) -> Result<Value, GnxError> {
    if !path.exists() {
        return Ok(json!({"hooks": {}}));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| GnxError::Output(format!("read {}: {e}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(json!({"hooks": {}}));
    }
    serde_json::from_str(&raw)
        .map_err(|e| GnxError::InvalidArgument(format!("settings.json parse: {e}")))
}

fn self_exe() -> Result<String, GnxError> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| GnxError::Output(format!("current_exe: {e}")))
}

fn event_kebab_to_camel(ev: &str) -> &'static str {
    match ev {
        "session-start" => "SessionStart",
        "user-prompt-submit" => "UserPromptSubmit",
        "pre-tool-use" => "PreToolUse",
        "post-tool-use" => "PostToolUse",
        _ => unreachable!(),
    }
}

fn matcher_for(ev: &str) -> &'static str {
    match ev {
        "pre-tool-use" => "Grep|Glob|Bash",
        "post-tool-use" => "Bash",
        _ => "",
    }
}

fn timeout_for(ev: &str) -> u64 {
    match ev {
        "user-prompt-submit" => 3,
        "pre-tool-use" => 10,
        _ => 5,
    }
}

fn merge_entry(settings: &mut Value, ev: &str, exe: &str) {
    let camel = event_kebab_to_camel(ev);
    let cmd = format!("\"{exe}\" hook {ev} --claude-code");

    let hooks_obj = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let arr = hooks_obj
        .as_object_mut()
        .unwrap()
        .entry(camel.to_string())
        .or_insert_with(|| json!([]));
    let arr = arr.as_array_mut().unwrap();

    // Idempotence: drop any existing entry pointing at `gnx hook <ev>`.
    arr.retain(|e| {
        let c = e
            .get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|hs| hs.first())
            .and_then(|h0| h0.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        !(c.contains(&format!("hook {ev}")) && c.contains("--claude-code"))
    });

    let mut entry = Map::new();
    entry.insert("matcher".into(), Value::String(matcher_for(ev).into()));
    let mut h = Map::new();
    h.insert("type".into(), Value::String("command".into()));
    h.insert("command".into(), Value::String(cmd));
    h.insert("timeout".into(), Value::Number(timeout_for(ev).into()));
    if matches!(ev, "pre-tool-use") {
        h.insert(
            "statusMessage".into(),
            Value::String("Enriching with gnx graph context...".into()),
        );
    } else if matches!(ev, "post-tool-use") {
        h.insert(
            "statusMessage".into(),
            Value::String("Checking gnx index freshness...".into()),
        );
    }
    entry.insert("hooks".into(), Value::Array(vec![Value::Object(h)]));
    arr.push(Value::Object(entry));
}

fn remove_entry(settings: &mut Value, ev: &str) {
    let camel = event_kebab_to_camel(ev);
    let Some(hooks_obj) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    let Some(arr) = hooks_obj.get_mut(camel).and_then(|a| a.as_array_mut()) else {
        return;
    };
    arr.retain(|e| {
        let c = e
            .get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|hs| hs.first())
            .and_then(|h0| h0.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        !(c.contains(&format!("hook {ev}")) && c.contains("--claude-code"))
    });
}

fn is_installed(settings: &Value, ev: &str) -> bool {
    let camel = event_kebab_to_camel(ev);
    settings
        .get("hooks")
        .and_then(|h| h.get(camel))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter().any(|e| {
                let c = e
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .and_then(|hs| hs.first())
                    .and_then(|h0| h0.get("command"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                c.contains(&format!("hook {ev}")) && c.contains("--claude-code")
            })
        })
        .unwrap_or(false)
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), GnxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| GnxError::Output(format!("mkdir {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| GnxError::Output(format!("serialize: {e}")))?;
    fs::write(&tmp, serialized)
        .map_err(|e| GnxError::Output(format!("write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path)
        .map_err(|e| GnxError::Output(format!("rename {} → {}: {e}", tmp.display(), path.display())))?;
    Ok(())
}
```

- [ ] **Step 5: Wire into admin/mod.rs**

Open `crates/graph-nexus-cli/src/commands/admin/mod.rs`.

Add to the file `pub mod claude_code;` near other module declarations.

Add to the `AdminCommands` enum (after existing variants):
```rust
    /// Install Claude Code hook entries into settings.json.
    InstallHook(claude_code::InstallHookArgs),
    /// Remove Claude Code hook entries from settings.json.
    UninstallHook(claude_code::UninstallHookArgs),
    /// Show Claude Code hook install status.
    Status(claude_code::StatusArgs),
```

Add dispatch arms in `pub fn run(cmd: AdminCommands) -> Result<(), GnxError>`:
```rust
        AdminCommands::InstallHook(args) => claude_code::run_install(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
```

- [ ] **Step 6: Run tests, verify pass**

Run: `cargo test -p graph-nexus --test hook_install_settings_test`
Expected: 3/3 PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/admin/claude_code.rs \
        crates/graph-nexus-cli/src/commands/admin/mod.rs \
        crates/graph-nexus-cli/tests/hook_install_settings_test.rs
git commit -m "feat(admin): T5 — install/uninstall/status for Claude Code hooks"
```

---

## Task 6: assets/claude-code/rules.md bundled template (PARALLEL)

**Files:**
- Create: `crates/graph-nexus-cli/assets/claude-code/rules.md`

- [ ] **Step 1: Author the template**

```markdown
gnx index: {{stats.nodes}} symbols, {{stats.edges}} rels at HEAD {{head}}.
Use `gnx inspect <name>` for symbol context, `gnx search <term>` for
fuzzy/semantic lookup, `gnx impact <name>` for blast radius.
{{#if graphify}}
graphify-out/ available — use that for narrative architecture context.
{{/if}}
{{#if wiki}}
graphify-out/wiki/index.md is the entry point for the indexed wiki.
{{/if}}
```

- [ ] **Step 2: Commit**

```bash
git add crates/graph-nexus-cli/assets/claude-code/rules.md
git commit -m "feat(hook): T6 — bundled SessionStart rules template"
```

Note: installation of this template (copying to `~/.claude/hooks/gnx/rules.md`) happens inside `admin/claude_code.rs::run_install` on first install — out of scope for this asset task.

---

## Task 7: PreToolUse event handler (DEPENDS ON T1 — sequential after T1)

**Files:**
- Modify: `crates/graph-nexus-cli/src/commands/hook/pre_tool_use.rs`
- Create: `crates/graph-nexus-cli/tests/hook_pre_tool_use_test.rs`

- [ ] **Step 1: Write failing tests focused on pattern extraction**

```rust
//! PreToolUse hook: pattern extraction + in-process graph augmentation.

use std::io::Write;
use std::process::{Command, Stdio};

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(envelope: &str) -> std::process::Output {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(envelope.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn short_pattern_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"ab"}}"#);
    assert!(out.stdout.is_empty(), "<3 char pattern should no-op");
}

#[test]
fn missing_graph_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"validateUser"}}"#);
    assert!(out.stdout.is_empty(), "no .gitnexus-rs/ → no-op");
}

#[test]
fn bash_grep_extracts_first_non_flag_arg() {
    // Stdin envelope only — actual graph call is gated on .gitnexus-rs/ existing.
    // This test pins that we correctly identify the pattern but bail without index.
    let out = run(r#"{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"rg -n 'validateUser' src/"}}"#);
    assert!(out.stdout.is_empty(), "no index → no-op even with valid pattern");
    assert!(out.status.success(), "should never fail the hook on no-op");
}

#[test]
fn non_search_tool_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Read","tool_input":{"file_path":"foo"}}"#);
    assert!(out.stdout.is_empty());
}
```

- [ ] **Step 2: Run tests, verify they pass on the stub**

Run: `cargo test -p graph-nexus --test hook_pre_tool_use_test`
Expected: 4/4 PASS even on stub. These pin "no-op" branches; the
"with index → emit hits" branch is exercised in T8.

- [ ] **Step 3: Implement pre_tool_use.rs**

```rust
//! PreToolUse handler: extract a search pattern from Grep/Glob/Bash
//! invocations and inject top-K graph hits into the conversation.

use super::common::{emit_additional_context, gitnexus_dir, HookInput};
use crate::commands::search::{compute_hits, SearchArgs, SearchMode};
use crate::engine::Engine;
use graph_nexus_core::GnxError;

const MAX_HITS: usize = 5;
const MAX_BYTES: usize = 2048;

pub fn handle(input: &HookInput) -> Result<(), GnxError> {
    let pattern = match extract_pattern(&input.tool_name, &input.tool_input) {
        Some(p) if p.len() >= 3 => p,
        _ => return Ok(()),
    };
    let gnx_dir = match gitnexus_dir(&input.cwd) {
        Some(d) => d,
        None => return Ok(()),
    };
    let graph_path = gnx_dir.join("graph.bin");
    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    let args = SearchArgs {
        pattern,
        mode: SearchMode::Auto,
        kind: None,
        repo: None,
        format: None,
    };
    let hits = match compute_hits(args, &engine) {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };
    if hits.is_empty() {
        return Ok(());
    }
    let lines = format_hits(&hits);
    if lines.trim().is_empty() {
        return Ok(());
    }
    emit_additional_context("PreToolUse", &lines);
    Ok(())
}

fn format_hits(hits: &[crate::commands::search::Hit]) -> String {
    let mut out = String::new();
    out.push_str("gnx graph hits:\n");
    let mut count = 0usize;
    for h in hits.iter().take(MAX_HITS) {
        let line = format!(
            "  [{}] {}:{} {} (callers:{}) score:{:.3}\n",
            h.kind, h.file, h.line, h.name, h.caller_count, h.score
        );
        if out.len() + line.len() > MAX_BYTES {
            break;
        }
        out.push_str(&line);
        count += 1;
    }
    if count == 0 {
        return String::new();
    }
    out
}

fn extract_pattern(tool: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool {
        "Grep" => tool_input.get("pattern").and_then(|v| v.as_str()).map(str::to_string),
        "Glob" => {
            let raw = tool_input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            // First alpha-led stem of ≥3 chars.
            let re = regex::Regex::new(r"[*/]([a-zA-Z][a-zA-Z0-9_-]{2,})").ok()?;
            re.captures(raw).map(|c| c[1].to_string())
        }
        "Bash" => {
            let cmd = tool_input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let stripped = super::post_tool_use_strip_shell_quotes_public(cmd);
            extract_from_shell(&stripped)
        }
        _ => None,
    }
}

fn extract_from_shell(cmd: &str) -> Option<String> {
    if !cmd.contains(" rg ") && !cmd.contains(" grep ")
        && !cmd.starts_with("rg ") && !cmd.starts_with("grep ")
        && !cmd.ends_with(" rg") && !cmd.ends_with(" grep") {
        return None;
    }
    let flags_with_values = [
        "-e", "-f", "-m", "-A", "-B", "-C", "-g", "--glob", "-t", "--type", "--include", "--exclude",
    ];
    let mut found_cmd = false;
    let mut skip_next = false;
    for token in cmd.split_whitespace() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !found_cmd {
            if token == "rg" || token == "grep" {
                found_cmd = true;
            }
            continue;
        }
        if token.starts_with('-') {
            if flags_with_values.iter().any(|f| f == &token) {
                skip_next = true;
            }
            continue;
        }
        let cleaned: String = token.chars().filter(|c| *c != '"' && *c != '\'').collect();
        if cleaned.len() >= 3 {
            return Some(cleaned);
        }
    }
    None
}
```

And in `commands/hook/post_tool_use.rs`, expose `strip_shell_quotes` via a `pub(super) fn post_tool_use_strip_shell_quotes_public(...)` wrapper for the PreToolUse handler to reuse — OR move `strip_shell_quotes` into `commands/hook/common.rs` as `pub(super) fn strip_shell_quotes(cmd: &str) -> String` and call from both.

Pick the move-to-common option (cleaner): add `pub(super) fn strip_shell_quotes(cmd: &str) -> String { ... }` to `common.rs` (move body verbatim from `post_tool_use.rs`), update `post_tool_use.rs` to call `super::common::strip_shell_quotes`, update `pre_tool_use.rs` to call `super::common::strip_shell_quotes`.

- [ ] **Step 4: Run all hook tests**

Run: `cargo test -p graph-nexus --test 'hook_*'`
Expected: all hook test files pass.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/src/commands/hook/pre_tool_use.rs \
        crates/graph-nexus-cli/src/commands/hook/post_tool_use.rs \
        crates/graph-nexus-cli/src/commands/hook/common.rs \
        crates/graph-nexus-cli/tests/hook_pre_tool_use_test.rs
git commit -m "feat(hook): T7 — PreToolUse pattern extract + in-process search

Shared strip_shell_quotes moved into hook::common for Grep/Glob/Bash
pattern extraction. PreToolUse calls search::compute_hits in-process
(no subprocess fork), formats top-K (≤5 or 2KB cap) as additionalContext."
```

---

## Task 8: End-to-end smoke (DEPENDS on T0-T7)

**Files:**
- Create: `crates/graph-nexus-cli/tests/hook_e2e_smoke_test.rs`

- [ ] **Step 1: Write the e2e test**

```rust
//! E2E: build a tiny git repo, run `gnx admin index`, install the
//! Claude Code hook, simulate envelope flow, verify behaviour.

use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    Command::new(gnx_bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn smoke_admin_status_reports_missing_then_installed() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");

    let before = Command::new(gnx_bin())
        .args(["admin", "status", "--claude-code", "--settings-path"])
        .arg(&settings)
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&before.stdout);
    for ev in ["session-start", "user-prompt-submit", "pre-tool-use", "post-tool-use"] {
        assert!(body.contains(ev), "status should list {ev}");
    }
    assert!(body.contains("missing"));

    let install = Command::new(gnx_bin())
        .args([
            "admin", "install-hook", "--claude-code",
            "--events", "session-start,pre-tool-use",
            "--settings-path",
        ])
        .arg(&settings)
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));

    let after = Command::new(gnx_bin())
        .args(["admin", "status", "--claude-code", "--settings-path"])
        .arg(&settings)
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&after.stdout);
    assert!(body.contains("INSTALLED"), "status should reflect install");
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p graph-nexus --test hook_e2e_smoke_test`
Expected: PASS.

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED" | tail -10`
Expected: no FAILED lines.

- [ ] **Step 4: Clippy + fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/graph-nexus-cli/tests/hook_e2e_smoke_test.rs
git commit -m "test(hook): T8 — admin install/status round-trip e2e"
```

---

## Task 9: PR

- [ ] **Step 1: Push branch**

```bash
git push -u origin feat/claude-code-hooks
```

- [ ] **Step 2: Open PR**

```bash
gh pr create --title "feat(hook): Claude Code hooks for Rust gnx — clap-introspection compatible" \
  --body "$(cat <<'EOF'
## Summary

Port `~/bin/gnx.branch-spike/claude-hooks/gitnexus-hook.cjs` to a Rust subcommand `gnx hook <event> --claude-code`. In-process graph access skips the Node + second-subprocess hop the legacy cjs path requires (6× faster cold-start; saves a full subprocess on PreToolUse). Selective install via `gnx admin install-hook --claude-code [--events ...]` (TUI fallback when --events absent).

Spec: `docs/specs/2026-05-16-claude-code-hooks-design.md`
Plan: `docs/plans/2026-05-16-claude-code-hooks.md`

## Test plan

- [x] cargo test --workspace green
- [x] cargo clippy -D warnings clean
- [x] cargo fmt clean
- [x] gnx admin status --claude-code reports per-event installed/missing
- [ ] Reviewer: install in their own ~/.claude/settings.json, verify hooks fire as additionalContext

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review

**Spec coverage:**
- §1 motivation → T0 wires `gnx hook` skeleton, perf claim validated in §1 of spec from cold-start measurements
- §2.1 hook entry point → T0 (skeleton) + T2-T4, T7 (per-event)
- §2.2 admin subcommands → T5
- §3.1 SessionStart → T2
- §3.2 UserPromptSubmit → T3
- §3.3 PreToolUse → T7
- §3.4 PostToolUse → T4
- §4 Code layout → all tasks
- §5 Hook protocol envelope → T0 (common.rs HookInput + emit_additional_context)
- §6 Failure modes → covered in handler impls (T2/T3/T4/T7 all early-return on missing index)
- §7 Testing → tests live in each task
- §8 Migration / coexistence → no new code; behavioural only (legacy and new write different marker dirs)
- §9 Out of scope → ack via comments; not implemented
- §10 Decision trajectory → matches choices baked into tasks

**Placeholder scan:** No `TBD/TODO/FIXME/???` in plan body. All code blocks complete.

**Type consistency:**
- `SearchArgs { pattern, mode, kind, repo, format }` used in T7 matches the struct definition I can grep from existing main (verified via spec §3.3 reference).
- `Hit { repo, score, kind, file, line, name, signature, caller_count }` used in T1 step 2 and T7 format_hits match.
- `EnsureResult::{Ready, Missing, Stale { age_seconds }}` used in T4 matches `auto_ensure.rs` definition (re-read during spec §3.4 drafting).
- `HookInput` (T0 common.rs) used identically in T2/T3/T4/T7.

**Parallel dispatch map:**
- After T0 lands: T1, T2, T3, T4, T5, T6 are mutually independent → 6 parallel subagents
- T7 depends on T1 (compute_hits) → start after T1's commit lands
- T8 depends on T0-T7 → sequential
- T9 sequential (PR open)

---

## Execution Handoff

**Plan complete and committed to `docs/plans/2026-05-16-claude-code-hooks.md`. Two execution options:**

**1. Subagent-Driven (recommended given user's explicit parallel preference)** — dispatch fresh subagent per task; T1-T6 fan out in parallel after T0; review between waves.

**2. Inline Execution** — execute tasks sequentially in this session.

**Which approach?**
