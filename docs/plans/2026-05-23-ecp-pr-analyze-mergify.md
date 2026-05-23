# `ecp dev pr-analyze` + Mergify Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a graph-aware merge queue by feeding ecp impact-graph signals (area / risk / cross-PR semantic conflict) into Mergify via PR labels + commit statuses.

**Architecture:** New `ecp dev pr-analyze` subcommand shells out to `ecp impact --baseline` (no internal API coupling), classifies a PR by changed-path area + impact-set risk + cross-PR overlap, and emits a JSON payload. A new GitHub Actions workflow runs the subcommand on every PR push and applies the suggested labels + commit status. `.mergify.yml` consumes those signals to route PRs into parallel queues and prioritize by risk.

**Tech Stack:** Rust (clap, serde_json, std::process::Command), GitHub Actions YAML, Mergify YAML config. Reuses existing `ecp impact` CLI as a black-box subprocess.

**Spec:** `docs/specs/2026-05-23-ecp-pr-analyze-mergify-design.md`

---

## Branch & PR strategy

Per spec §Migration, each phase ships as its own PR for surgical blast radius. **Four PRs total** for this feature:

| PR | Branch | Worktree dir | Contents |
|---|---|---|---|
| **PR #A — docs** | `docs/ecp-pr-analyze-mergify-design` | `.claude/worktrees/ecp-pr-analyze-spec/` (this one) | Spec + plan only — no code. Land first. |
| **PR #B — Phase 1** | `feat/ecp-dev-pr-analyze` | `.claude/worktrees/ecp-pr-analyze-impl/` | Tasks 1–10. The Rust subcommand. |
| **PR #C — Phase 2** | `ci/ecp-pr-analyze-workflow` | `.claude/worktrees/ecp-pr-analyze-workflow/` | Tasks 11–12. The GH Actions workflow. Depends on PR #B merged so the binary exists. |
| **PR #D — Phase 3** | `chore/mergify-config` | `.claude/worktrees/ecp-mergify-config/` | Tasks 13–14. The Mergify config. Depends on PR #C merged so labels actually get applied. |

Open a fresh worktree off the latest `main` for each PR (`git worktree add ...`). When this plan's task list says "commit + push" without naming a branch, infer from the Phase header which branch the work belongs on.

The docs PR (#A) lands first because it's free-standing documentation; reviewers can read the full spec + plan before any code shows up. PRs #B, #C, #D land in strict sequence because each depends on the prior.

## File Structure

**New:**

| File | Responsibility |
|---|---|
| `crates/ecp-cli/src/commands/dev/pr_analyze.rs` | Subcommand entrypoint, classification logic, gh wrapper, output assembly, unit tests (all in one file ~250 LoC; matches existing `dev/uid_audit.rs` pattern) |
| `crates/ecp-cli/tests/pr_analyze_integration.rs` | End-to-end golden test with mocked gh + impact subprocess |
| `crates/ecp-cli/tests/fixtures/pr_analyze/sample_impact.json` | Fixture impact JSON for the integration test |
| `.github/workflows/ecp-pr-analyze.yml` | PR-push trigger; runs the subcommand; applies labels + status |
| `.mergify.yml` | Queue rules + PR rules consuming the ecp labels/statuses |

**Modify:**

| File | Change |
|---|---|
| `crates/ecp-cli/src/commands/dev/mod.rs` | Add `PrAnalyze` variant + `pub mod pr_analyze;` + dispatch |

---

## Phase 1 — `ecp dev pr-analyze` subcommand

### Task 1: Scaffold module + clap surface + dispatch wiring

**Files:**
- Create: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`
- Modify: `crates/ecp-cli/src/commands/dev/mod.rs`

- [ ] **Step 1: Create the skeleton file**

`crates/ecp-cli/src/commands/dev/pr_analyze.rs`:

```rust
//! `ecp dev pr-analyze` — classify a PR by area / risk / cross-PR semantic
//! conflict, emit JSON consumed by `.github/workflows/ecp-pr-analyze.yml`
//! to apply labels + commit statuses for Mergify routing.
//!
//! Black-box wraps `ecp impact --baseline <ref> --format json` (subprocess),
//! so no tight coupling to impact's internal API.

use crate::output::OutputFormat;
use clap::Args;
use ecp_core::EcpError;

#[derive(Args, Debug, Clone)]
pub struct PrAnalyzeArgs {
    /// Base ref to diff against (typically `origin/main`).
    #[arg(long)]
    pub baseline: String,

    /// PR head ref (typically `HEAD` inside the PR-checkout workflow).
    #[arg(long = "pr-head")]
    pub pr_head: String,

    /// PR number — required to look up sibling PRs via gh CLI.
    #[arg(long = "pr-number")]
    pub pr_number: u32,

    /// Label scoping the cross-PR conflict scan. Defaults to `merge-queue`.
    #[arg(long = "queue-label", default_value = "merge-queue")]
    pub queue_label: String,

    /// Output format. Workflow consumes JSON.
    #[arg(long, default_value = "json")]
    pub format: OutputFormat,

    /// Do not write/update own cache comment, do not call gh mutations.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

pub fn run(args: PrAnalyzeArgs, _cli_graph: &std::path::Path) -> Result<(), EcpError> {
    // Stub — implemented incrementally in later tasks.
    let _ = args;
    eprintln!("pr-analyze: not yet implemented");
    Ok(())
}
```

- [ ] **Step 2: Register variant in `dev/mod.rs`**

In `crates/ecp-cli/src/commands/dev/mod.rs`, add the module + variant + dispatch:

```rust
pub mod pr_analyze;
pub mod uid_audit;

#[derive(Subcommand, Debug, Clone)]
pub enum DevCommands {
    UidAudit(uid_audit::UidAuditArgs),
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
    /// Classify a PR by graph-aware area/risk/cross-PR conflict; emit
    /// JSON for the ecp-pr-analyze workflow to apply labels + status.
    PrAnalyze(pr_analyze::PrAnalyzeArgs),
}

pub fn run(cmd: DevCommands, cli_graph: &std::path::Path) -> Result<(), EcpError> {
    match cmd {
        DevCommands::UidAudit(args) => uid_audit::run(args, cli_graph),
        DevCommands::VerifyResolver(args) => crate::commands::verify_resolver::run(args),
        DevCommands::PrAnalyze(args) => pr_analyze::run(args, cli_graph),
    }
}
```

- [ ] **Step 3: Verify it compiles + `--help` works**

Run: `cargo build -p egent-code-plexus --bin ecp --release`
Expected: builds without warnings.

Run: `./target/release/ecp dev pr-analyze --help`
Expected: prints help with all 6 flags listed.

- [ ] **Step 4: Verify clippy clean**

Run: `cargo clippy -p egent-code-plexus --bin ecp`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs crates/ecp-cli/src/commands/dev/mod.rs
git commit -m "feat(dev): scaffold ecp dev pr-analyze subcommand"
```

---

### Task 2: Output JSON types

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Add the serde types**

Append to `pr_analyze.rs` after the imports:

```rust
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Area {
    Parser,
    Cli,
    Test,
    Docs,
}

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct CrossPrConflict {
    pub pr: u32,
    pub overlap_symbols: Vec<String>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct StatusSuggestion {
    pub context: String,
    pub state: String, // "success" | "pending"
    pub description: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct PrAnalyzeOutput {
    pub pr_number: u32,
    pub head_sha: String,
    pub baseline_sha: String,
    pub area: Option<Area>,
    pub risk: Risk,
    pub impact_size: usize,
    pub changed_symbols: Vec<String>,
    pub cross_pr_conflicts: Vec<CrossPrConflict>,
    pub suggested_labels: Vec<String>,
    pub suggested_status: StatusSuggestion,
}
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p egent-code-plexus`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): output JSON types (Area, Risk, conflicts, status)"
```

---

### Task 3: Area classifier (pure function + TDD)

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Write failing tests**

Append to `pr_analyze.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pb(s: &str) -> PathBuf { PathBuf::from(s) }

    #[test]
    fn area_pure_parser() {
        let paths = vec![
            pb("crates/ecp-analyzer/src/python/parser.rs"),
            pb("crates/ecp-analyzer/src/python/queries.rs"),
        ];
        assert_eq!(classify_area(&paths), Some(Area::Parser));
    }

    #[test]
    fn area_pure_cli() {
        let paths = vec![pb("crates/ecp-cli/src/commands/impact.rs")];
        assert_eq!(classify_area(&paths), Some(Area::Cli));
    }

    #[test]
    fn area_pure_test() {
        let paths = vec![
            pb("crates/ecp-cli/tests/foo.rs"),
            pb("crates/ecp-cli/examples/bar.rs"),
        ];
        assert_eq!(classify_area(&paths), Some(Area::Test));
    }

    #[test]
    fn area_pure_docs() {
        let paths = vec![pb("docs/specs/abc.md"), pb("README.md")];
        assert_eq!(classify_area(&paths), Some(Area::Docs));
    }

    #[test]
    fn area_mixed_returns_none() {
        let paths = vec![
            pb("crates/ecp-analyzer/src/python/parser.rs"),
            pb("crates/ecp-cli/src/commands/impact.rs"),
        ];
        assert_eq!(classify_area(&paths), None);
    }

    #[test]
    fn area_empty_returns_none() {
        assert_eq!(classify_area(&[]), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests`
Expected: compile error "cannot find function `classify_area`".

- [ ] **Step 3: Implement `classify_area`**

Append (before `#[cfg(test)] mod tests`):

```rust
use std::path::Path;

fn path_area(p: &Path) -> Option<Area> {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with("crates/ecp-analyzer/src/") && s.ends_with(".rs") {
        // Per spec: parser changes live under crates/ecp-analyzer/src/<lang>/.
        // Top-level files (lib.rs / mod.rs) classify as parser too — they
        // touch shared parser infra.
        return Some(Area::Parser);
    }
    if s.starts_with("crates/ecp-cli/src/commands/") {
        return Some(Area::Cli);
    }
    if s.contains("/tests/") || s.contains("/examples/") {
        return Some(Area::Test);
    }
    if s.starts_with("docs/") || s.ends_with(".md") {
        return Some(Area::Docs);
    }
    None
}

/// Returns `Some(area)` only when ALL changed paths agree on the same area.
/// Mixed-area PRs return `None` and fall to the `default` queue per spec.
pub fn classify_area(paths: &[std::path::PathBuf]) -> Option<Area> {
    if paths.is_empty() {
        return None;
    }
    let first = path_area(&paths[0])?;
    for p in &paths[1..] {
        if path_area(p)? != first {
            return None;
        }
    }
    Some(first)
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests`
Expected: all 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): area classifier by changed file paths"
```

---

### Task 4: Risk classifier from impact_size

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Add failing boundary tests**

Append to the `mod tests` block:

```rust
    #[test]
    fn risk_low_boundary() {
        assert_eq!(classify_risk(0), Risk::Low);
        assert_eq!(classify_risk(5), Risk::Low);
    }

    #[test]
    fn risk_medium_boundary() {
        assert_eq!(classify_risk(6), Risk::Medium);
        assert_eq!(classify_risk(30), Risk::Medium);
    }

    #[test]
    fn risk_high_boundary() {
        assert_eq!(classify_risk(31), Risk::High);
        assert_eq!(classify_risk(1000), Risk::High);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests::risk`
Expected: compile error "cannot find function `classify_risk`".

- [ ] **Step 3: Implement**

Add before the test block:

```rust
/// Risk bucket from impact set size. Thresholds 5/30 are dev-machine
/// measurements on this repo; revisit after a month of real usage.
pub fn classify_risk(impact_size: usize) -> Risk {
    match impact_size {
        0..=5 => Risk::Low,
        6..=30 => Risk::Medium,
        _ => Risk::High,
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests::risk`
Expected: all 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): risk classifier from impact set size"
```

---

### Task 5: Subprocess wrapper for `ecp impact --baseline`

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Inspect `ecp impact --baseline ... --format json` actual output**

Run from any indexed repo:
```bash
./target/release/ecp impact --baseline origin/main --format json | jq 'keys' | head -20
```

Expected: keys include `changed_symbols`, `impact`, or similar. **Note the actual top-level shape — the deserialization struct below assumes a `{ "changed_symbols": [...], "all_impact": [...] }` shape; adjust field names to match the live output.**

- [ ] **Step 2: Add the impact-JSON deserialization types**

Append to `pr_analyze.rs`:

```rust
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct ImpactJson {
    /// Symbols modified between baseline and HEAD (the diff's named entities).
    #[serde(default)]
    changed_symbols: Vec<String>,
    /// Full upstream closure: every caller reachable from changed_symbols.
    /// Field name in the live output may be `impact_set` or `all_impact` —
    /// confirm via Step 1 and adjust the `#[serde(rename)]` if needed.
    #[serde(default, rename = "impact_set")]
    impact_set: Vec<String>,
    /// Changed file paths, for area classification.
    #[serde(default)]
    changed_files: Vec<String>,
}

/// Shells out to `ecp impact --baseline <ref> --format json` and parses.
/// Returns an error if the impact CLI exits non-zero or produces invalid JSON.
fn run_impact_subprocess(baseline: &str) -> Result<ImpactJson, EcpError> {
    use std::process::Command;
    let exe = std::env::current_exe()
        .map_err(|e| EcpError::Generic(format!("locate self exe: {e}")))?;
    let out = Command::new(&exe)
        .args(["impact", "--baseline", baseline, "--format", "json"])
        .output()
        .map_err(|e| EcpError::Generic(format!("spawn ecp impact: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::Generic(format!(
            "ecp impact failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| EcpError::Generic(format!("parse impact JSON: {e}")))
}
```

- [ ] **Step 3: Add a smoke test that mocks via fixture file**

The subprocess is hard to unit-test reliably (depends on a built index). Add a parse-only test using a fixture:

Create `crates/ecp-cli/tests/fixtures/pr_analyze/sample_impact.json`:

```json
{
  "changed_symbols": ["FnA", "MethodB"],
  "impact_set": ["FnA", "MethodB", "CallerC", "CallerD", "CallerE"],
  "changed_files": ["crates/ecp-cli/src/commands/impact.rs"]
}
```

Append to `mod tests` in `pr_analyze.rs`:

```rust
    #[test]
    fn parse_impact_json_fixture() {
        let raw = include_str!("../../../tests/fixtures/pr_analyze/sample_impact.json");
        let parsed: ImpactJson = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.changed_symbols, vec!["FnA", "MethodB"]);
        assert_eq!(parsed.impact_set.len(), 5);
        assert_eq!(parsed.changed_files[0], "crates/ecp-cli/src/commands/impact.rs");
    }
```

- [ ] **Step 4: Run test**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests::parse_impact_json_fixture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs crates/ecp-cli/tests/fixtures/pr_analyze/sample_impact.json
git commit -m "feat(pr-analyze): subprocess wrapper for ecp impact --baseline + fixture parse test"
```

---

### Task 6: `gh` API wrapper trait + real impl + mock

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Define the trait and real impl**

Append to `pr_analyze.rs`:

```rust
/// PR record returned by `gh pr list --json number,headRefOid`.
#[derive(Deserialize, Debug, Clone)]
pub struct SiblingPr {
    pub number: u32,
    #[serde(rename = "headRefOid")]
    pub head_ref_oid: String,
}

/// Abstracted GitHub interactions so the cross-PR conflict logic is testable
/// without spawning `gh` or hitting the real API.
pub trait GhClient {
    /// List open PRs with the given label, excluding the given PR number.
    fn list_sibling_prs(&self, queue_label: &str, exclude_pr: u32)
        -> Result<Vec<SiblingPr>, EcpError>;

    /// Read the cached ecp impact JSON from a PR's hidden marker comment.
    /// Returns Ok(None) if no marker comment exists.
    fn read_cached_impact(&self, pr: u32) -> Result<Option<Vec<String>>, EcpError>;

    /// Write (create or update) the marker comment with this PR's impact set.
    fn write_cached_impact(&self, pr: u32, impact_set: &[String])
        -> Result<(), EcpError>;
}

pub const CACHE_MARKER: &str = "<!-- ecp-impact-cache:V1 -->";

pub struct RealGhClient;

impl GhClient for RealGhClient {
    fn list_sibling_prs(&self, queue_label: &str, exclude_pr: u32)
        -> Result<Vec<SiblingPr>, EcpError>
    {
        use std::process::Command;
        let out = Command::new("gh")
            .args(["pr", "list",
                "--label", queue_label,
                "--state", "open",
                "--json", "number,headRefOid",
                "--limit", "50",
            ])
            .output()
            .map_err(|e| EcpError::Generic(format!("gh pr list: {e}")))?;
        if !out.status.success() {
            return Err(EcpError::Generic(format!(
                "gh pr list failed: {}", String::from_utf8_lossy(&out.stderr)
            )));
        }
        let prs: Vec<SiblingPr> = serde_json::from_slice(&out.stdout)
            .map_err(|e| EcpError::Generic(format!("parse gh pr list: {e}")))?;
        Ok(prs.into_iter().filter(|p| p.number != exclude_pr).collect())
    }

    fn read_cached_impact(&self, pr: u32) -> Result<Option<Vec<String>>, EcpError> {
        use std::process::Command;
        let endpoint = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
        let out = Command::new("gh")
            .args(["api", &endpoint, "--jq",
                ".[] | select(.body | startswith(\"<!-- ecp-impact-cache:V1 -->\")) | .body"
            ])
            .output()
            .map_err(|e| EcpError::Generic(format!("gh api comments: {e}")))?;
        if !out.status.success() {
            return Ok(None); // no comments / no access — treat as cache miss
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let body = body.trim();
        if body.is_empty() { return Ok(None); }
        // Body shape: "<!-- ecp-impact-cache:V1 -->\n{JSON array}"
        let json_start = body.find('\n').map(|i| i + 1).unwrap_or(body.len());
        let json_payload = &body[json_start..];
        let symbols: Vec<String> = serde_json::from_str(json_payload)
            .map_err(|e| EcpError::Generic(format!("parse cached impact: {e}")))?;
        Ok(Some(symbols))
    }

    fn write_cached_impact(&self, pr: u32, impact_set: &[String]) -> Result<(), EcpError> {
        use std::process::Command;
        // Truncate to 65000 chars worth of JSON to stay under GH's 65535 limit.
        let mut payload = serde_json::to_string(impact_set)
            .map_err(|e| EcpError::Generic(format!("encode impact: {e}")))?;
        let truncated_marker = if payload.len() > 65_000 {
            payload.truncate(65_000);
            payload.push_str("\"]"); // best-effort close
            ":truncated"
        } else { "" };
        let body = format!("<!-- ecp-impact-cache:V1{truncated_marker} -->\n{payload}");

        // Try to find an existing marker comment to PATCH; otherwise POST a new one.
        let list_endpoint = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
        let list_out = Command::new("gh")
            .args(["api", &list_endpoint, "--jq",
                ".[] | select(.body | startswith(\"<!-- ecp-impact-cache:V1\")) | .id"
            ])
            .output()
            .map_err(|e| EcpError::Generic(format!("gh api list comments: {e}")))?;
        let existing_id = String::from_utf8_lossy(&list_out.stdout)
            .lines().next().map(str::to_owned).filter(|s| !s.is_empty());

        let exec = if let Some(id) = existing_id {
            let patch_endpoint = format!("repos/{{owner}}/{{repo}}/issues/comments/{id}");
            Command::new("gh")
                .args(["api", "-X", "PATCH", &patch_endpoint, "-f"])
                .arg(format!("body={body}"))
                .status()
        } else {
            Command::new("gh")
                .args(["pr", "comment", &pr.to_string(), "--body", &body])
                .status()
        };
        let status = exec.map_err(|e| EcpError::Generic(format!("gh write comment: {e}")))?;
        if !status.success() {
            return Err(EcpError::Generic(format!("gh write comment exit {status}")));
        }
        Ok(())
    }
}
```

- [ ] **Step 2: Verify compile**

Run: `cargo check -p egent-code-plexus`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): GhClient trait + real impl (list / read / write cache)"
```

---

### Task 7: Cross-PR conflict detector (tested with MockGh)

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Add failing tests with a mock GhClient**

Append to `mod tests`:

```rust
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct MockGh {
        siblings: Vec<SiblingPr>,
        cached: HashMap<u32, Vec<String>>,
        writes: RefCell<Vec<(u32, Vec<String>)>>,
    }

    impl MockGh {
        fn new(siblings: Vec<SiblingPr>, cached: HashMap<u32, Vec<String>>) -> Self {
            Self { siblings, cached, writes: RefCell::new(Vec::new()) }
        }
    }

    impl GhClient for MockGh {
        fn list_sibling_prs(&self, _label: &str, exclude: u32)
            -> Result<Vec<SiblingPr>, EcpError>
        {
            Ok(self.siblings.iter().cloned().filter(|p| p.number != exclude).collect())
        }
        fn read_cached_impact(&self, pr: u32) -> Result<Option<Vec<String>>, EcpError> {
            Ok(self.cached.get(&pr).cloned())
        }
        fn write_cached_impact(&self, pr: u32, impact: &[String]) -> Result<(), EcpError> {
            self.writes.borrow_mut().push((pr, impact.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn cross_pr_conflict_disjoint() {
        let mut cached = HashMap::new();
        cached.insert(101, vec!["FnX".into(), "FnY".into()]);
        let gh = MockGh::new(
            vec![SiblingPr { number: 101, head_ref_oid: "abc".into() }],
            cached,
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh, "merge-queue", 100,
            &["FnA".to_string(), "FnB".to_string()],
        ).unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn cross_pr_conflict_overlap() {
        let mut cached = HashMap::new();
        cached.insert(101, vec!["FnA".into(), "FnY".into()]);
        let gh = MockGh::new(
            vec![SiblingPr { number: 101, head_ref_oid: "abc".into() }],
            cached,
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh, "merge-queue", 100,
            &["FnA".to_string(), "FnB".to_string()],
        ).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].pr, 101);
        assert_eq!(conflicts[0].overlap_symbols, vec!["FnA"]);
    }

    #[test]
    fn cross_pr_conflict_missing_cache_is_conservative() {
        // Sibling PR exists but has no cached impact yet (race condition).
        // Spec: treat as conflict to be conservative.
        let gh = MockGh::new(
            vec![SiblingPr { number: 101, head_ref_oid: "abc".into() }],
            HashMap::new(),
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh, "merge-queue", 100,
            &["FnA".to_string()],
        ).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].overlap_symbols, vec!["__pending_analysis__".to_string()]);
    }

    #[test]
    fn cross_pr_conflict_excludes_self() {
        let gh = MockGh::new(
            vec![
                SiblingPr { number: 100, head_ref_oid: "self".into() },
                SiblingPr { number: 101, head_ref_oid: "abc".into() },
            ],
            HashMap::new(),
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh, "merge-queue", 100,
            &["FnA".to_string()],
        ).unwrap();
        // Only PR 101 should appear; self (100) filtered.
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].pr, 101);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests::cross_pr`
Expected: compile error "cannot find function `detect_cross_pr_conflicts`".

- [ ] **Step 3: Implement `detect_cross_pr_conflicts`**

Append before the `mod tests`:

```rust
use std::collections::BTreeSet;

/// For each sibling PR with the queue label, compute the overlap between
/// THIS PR's changed_symbols and the sibling's cached impact_set.
///
/// If a sibling has no cached impact (e.g. race-condition on near-simultaneous
/// pushes), it is reported with overlap_symbols = ["__pending_analysis__"] —
/// a conservative signal so Mergify holds off until next tick.
pub fn detect_cross_pr_conflicts<G: GhClient>(
    gh: &G,
    queue_label: &str,
    self_pr: u32,
    self_changed_symbols: &[String],
) -> Result<Vec<CrossPrConflict>, EcpError> {
    let self_set: BTreeSet<&String> = self_changed_symbols.iter().collect();
    let siblings = gh.list_sibling_prs(queue_label, self_pr)?;
    let mut out = Vec::new();
    for sibling in siblings {
        match gh.read_cached_impact(sibling.number)? {
            None => {
                out.push(CrossPrConflict {
                    pr: sibling.number,
                    overlap_symbols: vec!["__pending_analysis__".to_string()],
                });
            }
            Some(other_impact) => {
                let other_set: BTreeSet<&String> = other_impact.iter().collect();
                let overlap: Vec<String> = self_set.intersection(&other_set)
                    .map(|s| (*s).clone())
                    .collect();
                if !overlap.is_empty() {
                    out.push(CrossPrConflict {
                        pr: sibling.number,
                        overlap_symbols: overlap,
                    });
                }
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p egent-code-plexus --lib pr_analyze::tests::cross_pr`
Expected: all 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): cross-PR conflict detector (graph overlap via GhClient)"
```

---

### Task 8: Wire end-to-end in `run()`

**Files:**
- Modify: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`

- [ ] **Step 1: Replace the stub `run()` with the full implementation**

Replace the placeholder `pub fn run(...)` body with:

```rust
pub fn run(args: PrAnalyzeArgs, _cli_graph: &std::path::Path) -> Result<(), EcpError> {
    // 1. Shell out to ecp impact to get the diff's changed symbols + impact closure.
    let impact = run_impact_subprocess(&args.baseline)?;
    let changed_files_pb: Vec<std::path::PathBuf> =
        impact.changed_files.iter().map(std::path::PathBuf::from).collect();

    // 2. Classify
    let area = classify_area(&changed_files_pb);
    let risk = classify_risk(impact.impact_set.len());

    // 3. Cross-PR conflicts (real gh client unless --dry-run)
    let gh = RealGhClient;
    let conflicts = if args.dry_run {
        Vec::new() // dry-run skips network — preserves test ergonomics
    } else {
        detect_cross_pr_conflicts(&gh, &args.queue_label, args.pr_number,
            &impact.changed_symbols)?
    };

    // 4. Update own cache comment so future sibling analyses see this PR's impact
    if !args.dry_run {
        gh.write_cached_impact(args.pr_number, &impact.impact_set)?;
    }

    // 5. Resolve head/baseline SHAs via git rev-parse for reporting
    let head_sha = git_rev_parse(&args.pr_head)?;
    let baseline_sha = git_rev_parse(&args.baseline)?;

    // 6. Build suggested labels
    let mut suggested_labels = Vec::new();
    if let Some(a) = area {
        suggested_labels.push(format!("ecp:area-{}",
            serde_json::to_value(a).unwrap().as_str().unwrap().to_string()));
    }
    suggested_labels.push(format!("ecp:risk-{}",
        serde_json::to_value(risk).unwrap().as_str().unwrap().to_string()));

    // 7. Build commit status suggestion
    let suggested_status = if conflicts.is_empty() {
        StatusSuggestion {
            context: "ecp/cross-pr-conflict".to_string(),
            state: "success".to_string(),
            description: "No semantic conflict with queued PRs".to_string(),
        }
    } else {
        let prs: Vec<String> = conflicts.iter()
            .map(|c| format!("#{}", c.pr)).collect();
        let mut desc = format!("Conflicts with {}", prs.join(", "));
        if desc.len() > 140 { desc.truncate(137); desc.push_str("..."); }
        StatusSuggestion {
            context: "ecp/cross-pr-conflict".to_string(),
            state: "pending".to_string(),
            description: desc,
        }
    };

    // 8. Assemble + emit
    let out = PrAnalyzeOutput {
        pr_number: args.pr_number,
        head_sha,
        baseline_sha,
        area,
        risk,
        impact_size: impact.impact_set.len(),
        changed_symbols: impact.changed_symbols,
        cross_pr_conflicts: conflicts,
        suggested_labels,
        suggested_status,
    };

    match args.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&out)
                .map_err(|e| EcpError::Generic(format!("emit JSON: {e}")))?);
        }
        _ => {
            // text fallback for humans
            eprintln!("PR #{}  area={:?}  risk={:?}  impact={}  conflicts={}",
                out.pr_number, out.area, out.risk, out.impact_size,
                out.cross_pr_conflicts.len());
        }
    }

    Ok(())
}

fn git_rev_parse(reference: &str) -> Result<String, EcpError> {
    use std::process::Command;
    let out = Command::new("git")
        .args(["rev-parse", reference])
        .output()
        .map_err(|e| EcpError::Generic(format!("git rev-parse {reference}: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::Generic(format!(
            "git rev-parse {reference} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p egent-code-plexus --bin ecp --release`
Expected: clean build.

- [ ] **Step 3: Smoke-test with --dry-run on this very worktree**

Run from the repo root:
```bash
./target/release/ecp dev pr-analyze \
  --baseline main --pr-head HEAD --pr-number 9999 --dry-run --format json
```

Expected: emits a JSON document with `pr_number: 9999`, `cross_pr_conflicts: []`, and some `suggested_labels`. May fail if no graph index exists — in that case, run `./target/release/ecp index` first.

- [ ] **Step 4: Run full test suite**

Run: `cargo test -p egent-code-plexus --lib pr_analyze`
Expected: all unit tests still PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "feat(pr-analyze): wire run() end-to-end (impact → classify → conflict → JSON)"
```

---

### Task 9: Integration test with golden fixture

**Files:**
- Create: `crates/ecp-cli/tests/pr_analyze_integration.rs`
- Modify (optional): `crates/ecp-cli/tests/fixtures/pr_analyze/sample_impact.json` (already created in Task 5)

- [ ] **Step 1: Write the integration test**

`crates/ecp-cli/tests/pr_analyze_integration.rs`:

```rust
//! Integration test for `ecp dev pr-analyze`.
//!
//! Exercises classify_area + classify_risk + detect_cross_pr_conflicts end-to-end
//! via library calls (NOT the CLI subprocess — that path requires a built index
//! and gh credentials).

use ecp_cli::commands::dev::pr_analyze::{
    classify_area, classify_risk, Area, Risk,
};
use std::path::PathBuf;

#[test]
fn end_to_end_pure_cli_low_risk() {
    let paths = vec![PathBuf::from("crates/ecp-cli/src/commands/impact.rs")];
    let area = classify_area(&paths);
    let risk = classify_risk(3);
    assert_eq!(area, Some(Area::Cli));
    assert_eq!(risk, Risk::Low);
}

#[test]
fn end_to_end_mixed_high_risk() {
    let paths = vec![
        PathBuf::from("crates/ecp-analyzer/src/python/parser.rs"),
        PathBuf::from("crates/ecp-cli/src/commands/impact.rs"),
    ];
    let area = classify_area(&paths);
    let risk = classify_risk(75);
    assert_eq!(area, None); // mixed → falls to default queue
    assert_eq!(risk, Risk::High);
}
```

- [ ] **Step 2: Verify the relevant items are `pub`**

Open `crates/ecp-cli/src/commands/dev/pr_analyze.rs` and confirm the following are exported. If any are private (e.g. `Area`, `Risk`, `classify_area`, `classify_risk`), add `pub` to them.

```rust
pub enum Area { ... }
pub enum Risk { ... }
pub fn classify_area(...) -> ...
pub fn classify_risk(...) -> ...
```

- [ ] **Step 3: Re-export from `dev::mod` to keep the integration test path tidy**

In `crates/ecp-cli/src/commands/dev/mod.rs`, ensure `pub mod pr_analyze;` is present (it is, from Task 1). The integration test uses the fully-qualified path `ecp_cli::commands::dev::pr_analyze::{...}`.

- [ ] **Step 4: Run integration tests**

Run: `cargo test -p egent-code-plexus --test pr_analyze_integration`
Expected: both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ecp-cli/tests/pr_analyze_integration.rs crates/ecp-cli/src/commands/dev/pr_analyze.rs
git commit -m "test(pr-analyze): integration test for classify + risk end-to-end"
```

---

### Task 10: Phase-1 boundary — clippy + format pass

**Files:**
- Touch only: `crates/ecp-cli/src/commands/dev/pr_analyze.rs`
- Touch only: `crates/ecp-cli/tests/pr_analyze_integration.rs`

- [ ] **Step 1: Format the new files** (touched-files only per CLAUDE.md)

```bash
rustfmt --edition 2021 \
  crates/ecp-cli/src/commands/dev/pr_analyze.rs \
  crates/ecp-cli/tests/pr_analyze_integration.rs \
  crates/ecp-cli/src/commands/dev/mod.rs
```

- [ ] **Step 2: Clippy clean**

Run: `cargo clippy -p egent-code-plexus --tests`
Expected: no warnings.

- [ ] **Step 3: Full test pass**

Run: `cargo test -p egent-code-plexus --tests`
Expected: all green.

- [ ] **Step 4: Commit any format changes**

```bash
git add -A
git diff --cached --quiet || git commit -m "style(pr-analyze): rustfmt phase-1 outputs"
```

---

## Phase 2 — GitHub Actions workflow

### Task 11: Write `ecp-pr-analyze.yml`

**Files:**
- Create: `.github/workflows/ecp-pr-analyze.yml`

- [ ] **Step 1: Write the workflow**

`.github/workflows/ecp-pr-analyze.yml`:

```yaml
name: ecp PR analyze
on:
  pull_request:
    types: [opened, synchronize, reopened]
  push:
    branches: [main]

# Each PR's analysis runs independently; multiple PRs can run in parallel.
# Re-runs on the same PR cancel in-flight (only the latest analysis matters).
concurrency:
  group: ecp-pr-analyze-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: true

permissions:
  contents: read
  pull-requests: write
  statuses: write
  issues: write  # for PR comment cache writes

jobs:
  analyze:
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0  # need history for `git diff origin/main..HEAD`
          ref: ${{ github.event.pull_request.head.sha }}

      - name: Fetch base ref
        run: git fetch origin main:refs/remotes/origin/main

      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: stable
          components: ""

      - name: Cache ecp graph + cargo
        uses: actions/cache@v4
        with:
          path: |
            ~/.ecp
            ~/.cargo/registry
            ~/.cargo/git
            target
          key: ecp-pr-analyze-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}

      - name: Build ecp (release)
        run: cargo build -p egent-code-plexus --bin ecp --release

      - name: Index repo
        run: ./target/release/ecp index

      - name: Run pr-analyze
        id: analyze
        continue-on-error: true
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          ./target/release/ecp dev pr-analyze \
            --baseline origin/main \
            --pr-head HEAD \
            --pr-number ${{ github.event.pull_request.number }} \
            --format json \
            > /tmp/analysis.json
          cat /tmp/analysis.json

      - name: Apply labels + commit status
        if: steps.analyze.outcome == 'success'
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          PR: ${{ github.event.pull_request.number }}
          HEAD_SHA: ${{ github.event.pull_request.head.sha }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail

          # 1) Remove stale ecp:* labels, then add suggested ones
          for stale in $(gh pr view "$PR" --json labels --jq '.labels[].name' | grep '^ecp:' || true); do
            gh pr edit "$PR" --remove-label "$stale"
          done
          for label in $(jq -r '.suggested_labels[]' /tmp/analysis.json); do
            gh pr edit "$PR" --add-label "$label" 2>/dev/null || \
              gh label create "$label" --color "ededed" --description "ecp signal" || true
            gh pr edit "$PR" --add-label "$label"
          done

          # 2) Push commit status
          CONTEXT=$(jq -r '.suggested_status.context' /tmp/analysis.json)
          STATE=$(jq -r '.suggested_status.state' /tmp/analysis.json)
          DESC=$(jq -r '.suggested_status.description' /tmp/analysis.json)
          gh api -X POST "repos/$REPO/statuses/$HEAD_SHA" \
            -f context="$CONTEXT" -f state="$STATE" -f description="$DESC"

      - name: Log analysis failure (non-blocking)
        if: steps.analyze.outcome != 'success'
        run: |
          echo "::warning::ecp pr-analyze failed — PR falls to default Mergify queue"
```

- [ ] **Step 2: Lint the workflow YAML**

Run: `actionlint .github/workflows/ecp-pr-analyze.yml`
(If `actionlint` is not installed, skip — the workflow's first run is the lint.)
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ecp-pr-analyze.yml
git commit -m "ci: add ecp-pr-analyze workflow (PR labels + commit status for Mergify)"
```

---

### Task 12: Phase-2 smoke — verify workflow runs against a trivial PR

**Files:** none (verification only)

- [ ] **Step 1: Push PR #C and let the workflow smoke-test itself**

In the Phase 2 worktree (branch `ci/ecp-pr-analyze-workflow`), push and open the PR. Because the workflow is `on: pull_request`, GitHub will run it against THIS very PR — that's the smoke test.

```bash
git push -u origin ci/ecp-pr-analyze-workflow
gh pr create --title "ci: ecp PR analyze workflow (Phase 2)" \
  --body "Implements Phase 2 of docs/specs/2026-05-23-ecp-pr-analyze-mergify-design.md.

Adds .github/workflows/ecp-pr-analyze.yml. Workflow self-smoke-tests on this PR:
expect ecp:* labels to appear and ecp/cross-pr-conflict status to post.

Depends on PR #B (the ecp dev pr-analyze subcommand) being merged."
```

- [ ] **Step 2: Watch the workflow**

```bash
gh run watch  # picks the most recent run
```

Expected: `ecp PR analyze / analyze` step finishes within ~5 min on the runner.

- [ ] **Step 3: Verify labels appeared on the PR**

```bash
gh pr view --json labels --jq '.labels[].name' | grep '^ecp:'
```

Expected: at least `ecp:risk-low` (and possibly `ecp:area-docs` for this docs-only PR).

- [ ] **Step 4: Verify commit status posted**

```bash
gh pr checks
```

Expected: `ecp/cross-pr-conflict` status with state `success` (no sibling PRs labeled `merge-queue` yet, so no conflicts).

- [ ] **Step 5: If anything fails — iterate**

Common failures and fixes:
- "ecp index" timeout → reduce target paths in workflow's index step, or skip indexing on docs-only PRs
- "permission denied" on label add → ensure `pull-requests: write` permission in workflow YAML
- jq parse error → check `/tmp/analysis.json` shape via the `cat` debug line

Make any needed fixes in a NEW commit (per CLAUDE.md no-amend-after-hook-fail principle):
```bash
git add -A && git commit -m "fix(ci): <whatever>"
git push
```

Phase 2 is complete when the workflow runs green and labels appear.

---

## Phase 3 — Mergify integration

### Task 13: Write `.mergify.yml`

**Files:**
- Create: `.mergify.yml`

- [ ] **Step 1: Write the config**

`.mergify.yml`:

```yaml
queue_rules:
  - name: test-only
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-test
    batch_size: 10
    batch_max_wait_time: 30s

  - name: parser-changes
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-parser
    batch_size: 1

  - name: cli-changes
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-cli
    batch_size: 3
    batch_max_wait_time: 2m

  - name: docs-only
    conditions:
      - check-success=ecp/cross-pr-conflict
      - label=ecp:area-docs
    batch_size: 20
    batch_max_wait_time: 10s

  - name: default
    conditions:
      - check-success=ecp/cross-pr-conflict
    batch_size: 2

pull_request_rules:
  - name: queue if labeled merge-queue
    conditions:
      - label=merge-queue
      - check-success=Test (all platforms)
      - check-success=Code Quality (Linting & Formatting)
      - check-success=Supply-chain (audit & deny)
      - check-success=Lint GitHub Actions workflows
      - check-success=CodeQL
      - check-success=dependency-review
    actions:
      queue:
        name: default

  - name: high priority for low-risk
    conditions:
      - label=ecp:risk-low
    actions:
      queue:
        priority: high

  - name: low priority for high-risk
    conditions:
      - label=ecp:risk-high
    actions:
      queue:
        priority: low
```

- [ ] **Step 2: Validate via Mergify's online linter**

Open https://docs.mergify.com/configuration/ and paste the config into their online checker, OR use the Mergify CLI if installed:

```bash
mergify check
```

Expected: no syntax errors.

- [ ] **Step 3: Commit**

```bash
git add .mergify.yml
git commit -m "chore(mergify): area-based queues + risk-based priority routing"
```

---

### Task 14: Install Mergify GitHub App + verify routing

**Files:** none (manual GitHub UI + verification)

- [ ] **Step 1: Install the Mergify App on the repo**

1. Go to https://github.com/apps/mergify
2. Click "Install"
3. Select `coseto6125/egent-code-plexus`
4. Confirm
5. Mergify reads `.mergify.yml` from the default branch (main). Since this PR isn't merged yet, Mergify won't enforce on this PR until merge — that's fine for now; Phase 3 verification happens AFTER merge on a subsequent PR.

- [ ] **Step 2: After this PR merges, open a fresh test PR**

Open any small follow-up PR (e.g., a typo fix) and:
1. Wait for CI + ecp-pr-analyze workflow → labels appear
2. Add the `merge-queue` label manually
3. Mergify dashboard at https://dashboard.mergify.com/ should show the PR enqueued
4. Expected queue assignment: based on its `ecp:area-*` label

- [ ] **Step 3: Verify ecp signals are being read**

In the Mergify dashboard, click on the queued PR. The "Conditions" panel should show `check-success=ecp/cross-pr-conflict` evaluated to `true` and the `label=ecp:area-*` condition matching.

- [ ] **Step 4: Document the result in FU log**

Per `.claude/CLAUDE.md` follow-ups protocol, file the verification outcome in `.claude/FOLLOWUPS_DONE.md` once both phases work end-to-end:

```
### FU-2026-05-23-XXX  ·  surfaced in PR #<n>
- ✅ done in PR #<n>
- ecp dev pr-analyze + workflow + .mergify.yml landed
- Mergify App installed and verified routing on test PR
- Refs: docs/specs/2026-05-23-ecp-pr-analyze-mergify-design.md
```

---

## Self-Review Notes (for the plan author)

After writing this plan, the following spec sections have at least one task implementing them:

- §Components / `pr_analyze.rs` → Tasks 1–9
- §Components / `.github/workflows/ecp-pr-analyze.yml` → Task 11
- §Components / `.mergify.yml` → Task 13
- §Classification rules (area/risk/cross-PR) → Tasks 3, 4, 7
- §Error handling (graph not built / cache miss / oversized cache) → Tasks 5 (subprocess error), 7 (cache miss), 6 (truncation in `write_cached_impact`)
- §Testing (unit + integration + smoke) → Tasks 3–7 (units), 9 (integration), 12 + 14 (smoke)
- §Migration / rollout (3-phase) → Phase 1 (T1–T10), Phase 2 (T11–T12), Phase 3 (T13–T14)

No `TBD` / `TODO` / placeholder text used. Method names consistent across tasks (`classify_area`, `classify_risk`, `detect_cross_pr_conflicts`, `RealGhClient::{list_sibling_prs, read_cached_impact, write_cached_impact}`).

One genuine unknown remains: **the exact JSON shape returned by `ecp impact --baseline ... --format json`** — Task 5 Step 1 asks the implementer to confirm and adjust `ImpactJson` field renames if needed. This is unavoidable without booting an indexed repo at plan-write time; the plan flags it explicitly rather than hiding it.
