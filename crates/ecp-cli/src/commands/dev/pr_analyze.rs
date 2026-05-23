//! `ecp dev pr-analyze` — classify a PR by area / risk / cross-PR semantic
//! conflict, emit JSON consumed by `.github/workflows/ecp-pr-analyze.yml`
//! to apply labels + commit statuses for Mergify routing.
//!
//! Black-box wraps `ecp impact --baseline <ref> --format json` (subprocess),
//! so no tight coupling to impact's internal API.

use crate::git::safe_exec;
use crate::output::OutputFormat;
use clap::Args;
use ecp_core::EcpError;
use serde::{Deserialize, Serialize};
use std::path::Path;

const CACHE_MARKER: &str = "<!-- ecp-impact-cache:V1 -->";
// Prefix form (no trailing ` -->`): used by jq `startswith` to match both
// the plain marker and the `:truncated` suffix variant written on overflow.
const CACHE_MARKER_PREFIX: &str = "<!-- ecp-impact-cache:V1";

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Area {
    Parser,
    Cli,
    Test,
    Docs,
}

impl Area {
    pub fn to_kebab(self) -> &'static str {
        match self {
            Area::Parser => "parser",
            Area::Cli => "cli",
            Area::Test => "test",
            Area::Docs => "docs",
        }
    }
}

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn to_kebab(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct CrossPrConflict {
    pub pr: u32,
    pub overlap_symbols: Vec<String>,
}

#[derive(Serialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum CommitState {
    Success,
    Pending,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct StatusSuggestion {
    pub context: String,
    pub state: CommitState,
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

// ── impact subprocess types ──────────────────────────────────────────────────

/// One symbol that changed between baseline and HEAD.
/// Fields match the live `ecp impact --baseline --format json` shape.
#[derive(Deserialize, Debug)]
struct ChangedSymbol {
    pub name: String,
    /// `"Function"`, `"Method"`, `"Struct"`, `"Module"`, etc.
    #[allow(dead_code)]
    pub kind: String,
    /// Repo-relative path (forward slashes). No longer consumed (area
    /// classification uses `git diff --name-only` for comment-only-diff
    /// coverage) but kept so the deserializer doesn't fail on the field.
    #[serde(rename = "filePath")]
    #[allow(dead_code)]
    pub file_path: String,
    #[allow(dead_code)]
    pub line: u32,
    #[allow(dead_code)]
    pub change_type: String,
}

/// One entry inside `impact_by_symbol[*].impact`.
#[derive(Deserialize, Debug)]
struct ImpactEntry {
    pub name: String,
    /// 0 = the changed symbol itself; >0 = transitive callers.
    pub depth: u32,
}

/// Per-symbol BFS result emitted by `ecp impact --baseline`.
#[derive(Deserialize, Debug)]
struct ImpactBySymbol {
    #[allow(dead_code)]
    pub symbol: String,
    #[allow(dead_code)]
    #[serde(rename = "filePath", default)]
    pub file_path: String,
    #[serde(default)]
    pub impact: Vec<ImpactEntry>,
}

/// Top-level JSON envelope from `ecp impact --baseline <ref> --format json`.
///
/// Live shape (confirmed from source + runtime probe):
/// ```json
/// {
///   "status": "success",
///   "baseline": "<ref>",
///   "changed_symbols": [ { "name", "kind", "filePath", "line", "change_type" } ],
///   "impact_by_symbol": [ { "symbol", "filePath", "impact": [ { "name", "depth", ... } ] } ],
///   "hidden_heuristic_edges": 0
/// }
/// ```
/// There is no flat `impact_set` or `changed_files` field; those must be
/// derived from `changed_symbols[*].file_path` and
/// `impact_by_symbol[*].impact[depth>0]` respectively.
// Will be wired into `run()` in a later task.
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct ImpactJson {
    /// Symbols whose source body changed between baseline and HEAD.
    #[serde(default)]
    pub changed_symbols: Vec<ChangedSymbol>,
    /// Per-changed-symbol BFS results (callers reachable upstream).
    #[serde(default)]
    pub impact_by_symbol: Vec<ImpactBySymbol>,
}

impl ImpactJson {
    /// Files that contain at least one *semantically* changed symbol.
    /// Distinct from `git_diff_files(...)` (used by `run()` for area
    /// classification): symbol-derived view skips whitespace-only and
    /// comment-only diffs, where the git-diff view includes them. PR #390
    /// switched `run()` to git-diff so docs-only PRs classify correctly,
    /// but this view stays in the library surface so future LLM consumers
    /// can ask "which files actually had code changes?" without re-deriving
    /// from `changed_symbols` themselves. Mirrors `impact_set_names` /
    /// `changed_symbol_names` for derived-view symmetry on ImpactJson.
    #[allow(dead_code)]
    pub fn changed_files(&self) -> Vec<String> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        self.changed_symbols
            .iter()
            .filter(|s| seen.insert(s.file_path.as_str()))
            .map(|s| s.file_path.clone())
            .collect()
    }

    /// All symbol names reachable from changed symbols (depth > 0), deduplicated.
    /// Used to size the impact set for risk classification.
    pub fn impact_set_names(&self) -> Vec<String> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        self.impact_by_symbol
            .iter()
            .flat_map(|entry| entry.impact.iter())
            .filter(|e| e.depth > 0 && seen.insert(e.name.as_str()))
            .map(|e| e.name.clone())
            .collect()
    }

    /// Symbol names that directly changed (for the output `changed_symbols` list).
    pub fn changed_symbol_names(&self) -> Vec<String> {
        self.changed_symbols
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }
}

/// Shells out to `ecp impact --baseline <ref> --format json` and parses.
/// Returns an error if the impact CLI exits non-zero or produces invalid JSON.
fn run_impact_subprocess(baseline: &str) -> Result<ImpactJson, EcpError> {
    use std::process::Command;
    let exe = std::env::current_exe().map_err(EcpError::Io)?;
    let out = Command::new(&exe)
        .args(["impact", "--baseline", baseline, "--format", "json"])
        .output()
        .map_err(EcpError::Io)?;
    if !out.status.success() {
        return Err(EcpError::GitDiff {
            reason: format!(
                "ecp impact failed (exit {}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        });
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| EcpError::Serialization(format!("parse impact JSON: {e}")))
}

// ────────────────────────────────────────────────────────────────────────────

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

    /// Target branch of this PR. Sibling PRs targeting a different branch
    /// are excluded from cross-PR conflict scan — Mergify batches per target
    /// branch and we don't want a PR to develop to spuriously block a PR
    /// to main on overlapping symbols.
    #[arg(long = "base-branch", default_value = "main")]
    pub base_branch: String,

    /// Output format. Workflow consumes JSON.
    #[arg(long, default_value = "json")]
    pub format: OutputFormat,

    /// Do not write/update own cache comment, do not call gh mutations.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

pub fn run(args: PrAnalyzeArgs, _cli_graph: &std::path::Path) -> Result<(), EcpError> {
    // 1. Shell out to ecp impact to get changed symbols + impact closure.
    //    Comment-only or whitespace-only diffs return 0 changed_symbols, so
    //    impact.changed_files() (which derives from changed_symbols[].filePath)
    //    would be empty even when files DID change — pull the file list
    //    directly from git instead so area classification still works for
    //    docs/comment-only PRs.
    let impact = run_impact_subprocess(&args.baseline)?;
    // PR #390: use git diff so comment-only / whitespace-only changes (which
    // produce zero `changed_symbols`) still classify into the right area.
    // FU-043: `classify_area` is now generic over `AsRef<Path>` so the
    // `Vec<String>` from git diff can feed it without a `Vec<PathBuf>`
    // shim allocation.
    let changed_files = git_diff_files(&args.baseline, &args.pr_head)?;

    // 2. Classify
    let area = classify_area(&changed_files);
    let impact_set_names = impact.impact_set_names();
    let risk = classify_risk(impact_set_names.len());

    // 3. Cross-PR conflicts (real gh client unless --dry-run)
    let changed_symbol_names = impact.changed_symbol_names();
    let gh = RealGhClient;
    let conflicts = if args.dry_run {
        Vec::new() // dry-run skips network — preserves test ergonomics
    } else {
        detect_cross_pr_conflicts(
            &gh,
            &args.queue_label,
            &args.base_branch,
            args.pr_number,
            &changed_symbol_names,
        )?
    };

    // 4. Update own cache comment so future sibling analyses see this PR's impact
    if !args.dry_run {
        gh.write_cached_impact(args.pr_number, &impact_set_names)?;
    }

    // 5. Resolve head/baseline SHAs via git rev-parse for reporting
    let head_sha = git_rev_parse(&args.pr_head)?;
    let baseline_sha = git_rev_parse(&args.baseline)?;

    // 6. Build suggested labels
    let mut suggested_labels = Vec::new();
    if let Some(a) = area {
        suggested_labels.push(format!("ecp:area-{}", a.to_kebab()));
    }
    suggested_labels.push(format!("ecp:risk-{}", risk.to_kebab()));

    // 7. Build commit status suggestion
    let suggested_status = if conflicts.is_empty() {
        StatusSuggestion {
            context: "ecp/cross-pr-conflict".to_string(),
            state: CommitState::Success,
            description: "No semantic conflict with queued PRs".to_string(),
        }
    } else {
        let prs: Vec<String> = conflicts.iter().map(|c| format!("#{}", c.pr)).collect();
        let mut desc = format!("Conflicts with {}", prs.join(", "));
        if desc.len() > 140 {
            desc.truncate(137);
            desc.push_str("...");
        }
        StatusSuggestion {
            context: "ecp/cross-pr-conflict".to_string(),
            state: CommitState::Pending,
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
        impact_size: impact_set_names.len(),
        changed_symbols: changed_symbol_names,
        cross_pr_conflicts: conflicts,
        suggested_labels,
        suggested_status,
    };

    match args.format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&out)
                    .map_err(|e| EcpError::Serialization(format!("emit JSON: {e}")))?
            );
        }
        OutputFormat::Llm | OutputFormat::Toon | OutputFormat::Text => {
            return Err(EcpError::InvalidArgument(format!(
                "pr-analyze only supports --format json (got {:?}); the output is consumed by Mergify CI hooks which require JSON",
                args.format
            )));
        }
    }

    Ok(())
}

fn git_rev_parse(reference: &str) -> Result<String, EcpError> {
    let out = safe_exec::git()
        .args(["rev-parse", reference])
        .output()
        .map_err(EcpError::Io)?;
    if !out.status.success() {
        return Err(EcpError::GitDiff {
            reason: format!(
                "git rev-parse {reference} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Lists files changed between baseline..pr_head, regardless of whether the
/// diff carried semantic symbol changes. Needed because comment-only edits
/// (typo fixes, doc tweaks, WHY-comment adds) don't produce changed_symbols
/// in `ecp impact` output but DO change files — `classify_area` still needs
/// to know the touched paths to assign the right Mergify queue.
fn git_diff_files(baseline: &str, pr_head: &str) -> Result<Vec<String>, EcpError> {
    let out = safe_exec::git()
        .args(["diff", "--name-only", &format!("{baseline}..{pr_head}")])
        .output()
        .map_err(EcpError::Io)?;
    if !out.status.success() {
        return Err(EcpError::GitDiff {
            reason: format!(
                "git diff --name-only {baseline}..{pr_head}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

fn path_area(p: &Path) -> Option<Area> {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with("crates/ecp-analyzer/src/") && s.ends_with(".rs") {
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
pub fn classify_area<P: AsRef<Path>>(paths: &[P]) -> Option<Area> {
    if paths.is_empty() {
        return None;
    }
    let first = path_area(paths[0].as_ref())?;
    for p in &paths[1..] {
        if path_area(p.as_ref())? != first {
            return None;
        }
    }
    Some(first)
}

/// Risk bucket from impact set size. Thresholds 5/30 are dev-machine
/// measurements on this repo; revisit after a month of real usage.
pub fn classify_risk(impact_size: usize) -> Risk {
    match impact_size {
        0..=5 => Risk::Low,
        6..=30 => Risk::Medium,
        _ => Risk::High,
    }
}

// ── gh API wrapper ───────────────────────────────────────────────────────────

/// PR record returned by `gh pr list --json number,headRefOid`.
#[derive(Deserialize, Debug, Clone)]
pub struct SiblingPr {
    pub number: u32,
    #[serde(rename = "headRefOid")]
    #[allow(dead_code)]
    pub head_ref_oid: String,
}

/// Abstracted GitHub interactions so the cross-PR conflict logic is testable
/// without spawning `gh` or hitting the real API.
pub trait GhClient {
    /// List open PRs with the given label and target branch, excluding the
    /// given PR number. `base_branch` filters by target — siblings targeting
    /// a different branch are out-of-scope for cross-PR conflict.
    fn list_sibling_prs(
        &self,
        queue_label: &str,
        base_branch: &str,
        exclude_pr: u32,
    ) -> Result<Vec<SiblingPr>, EcpError>;

    /// Read the cached ecp impact JSON from a PR's hidden marker comment.
    /// Returns Ok(None) if no marker comment exists.
    fn read_cached_impact(&self, pr: u32) -> Result<Option<Vec<String>>, EcpError>;

    /// Write (create or update) the marker comment with this PR's impact set.
    fn write_cached_impact(&self, pr: u32, impact_set: &[String]) -> Result<(), EcpError>;
}

use std::collections::BTreeSet;

/// For each sibling PR with the queue label, compute the overlap between
/// THIS PR's changed_symbols and the sibling's cached impact_set.
///
/// If a sibling has no cached impact (e.g. race on near-simultaneous pushes,
/// or first sibling in a new batch), it is SKIPPED — not reported as a
/// conflict. Reporting missing-cache as conflict creates a deadlock when
/// N PRs push together: each sees N-1 uncached siblings and blocks itself.
/// Mergify's speculative trial catches any actual conflict at test time,
/// so a missed cache here is a graph-aware-skip not a real safety hole.
pub fn detect_cross_pr_conflicts<G: GhClient>(
    gh: &G,
    queue_label: &str,
    base_branch: &str,
    self_pr: u32,
    self_changed_symbols: &[String],
) -> Result<Vec<CrossPrConflict>, EcpError> {
    let self_set: BTreeSet<&String> = self_changed_symbols.iter().collect();
    let siblings = gh.list_sibling_prs(queue_label, base_branch, self_pr)?;
    let mut out = Vec::new();
    for sibling in siblings {
        match gh.read_cached_impact(sibling.number)? {
            None => {
                // No cache yet — don't block on graph-unknown. Mergify's
                // speculative trial catches real conflicts at test time.
                continue;
            }
            Some(other_impact) => {
                let other_set: BTreeSet<&String> = other_impact.iter().collect();
                let overlap: Vec<String> = self_set
                    .intersection(&other_set)
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

pub struct RealGhClient;

impl GhClient for RealGhClient {
    fn list_sibling_prs(
        &self,
        queue_label: &str,
        base_branch: &str,
        exclude_pr: u32,
    ) -> Result<Vec<SiblingPr>, EcpError> {
        use std::process::Command;
        let out = Command::new("gh")
            .args([
                "pr",
                "list",
                "--label",
                queue_label,
                "--base",
                base_branch,
                "--state",
                "open",
                "--json",
                "number,headRefOid",
                "--limit",
                "50",
            ])
            .output()
            .map_err(EcpError::Io)?;
        if !out.status.success() {
            return Err(EcpError::GitDiff {
                reason: format!(
                    "gh pr list failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            });
        }
        let prs: Vec<SiblingPr> = serde_json::from_slice(&out.stdout)
            .map_err(|e| EcpError::Serialization(format!("parse gh pr list: {e}")))?;
        Ok(prs.into_iter().filter(|p| p.number != exclude_pr).collect())
    }

    fn read_cached_impact(&self, pr: u32) -> Result<Option<Vec<String>>, EcpError> {
        use std::process::Command;
        let endpoint = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
        let jq_filter = format!(".[] | select(.body | startswith(\"{CACHE_MARKER}\")) | .body");
        let out = Command::new("gh")
            .args(["api", &endpoint, "--jq", &jq_filter])
            .output()
            .map_err(EcpError::Io)?;
        if !out.status.success() {
            return Ok(None); // no comments / no access — treat as cache miss
        }
        let body = String::from_utf8_lossy(&out.stdout);
        let body = body.trim();
        if body.is_empty() {
            return Ok(None);
        }
        // Body shape: "<!-- ecp-impact-cache:V1 -->\n{JSON array}"
        let json_start = body.find('\n').map(|i| i + 1).unwrap_or(body.len());
        let json_payload = &body[json_start..];
        let symbols: Vec<String> = serde_json::from_str(json_payload)
            .map_err(|e| EcpError::Serialization(format!("parse cached impact: {e}")))?;
        Ok(Some(symbols))
    }

    fn write_cached_impact(&self, pr: u32, impact_set: &[String]) -> Result<(), EcpError> {
        use std::process::Command;
        // Truncate to 65000 chars worth of JSON to stay under GH's 65535 limit.
        let mut payload = serde_json::to_string(impact_set)
            .map_err(|e| EcpError::Serialization(format!("encode impact: {e}")))?;
        // CACHE_MARKER is "<!-- ecp-impact-cache:V1 -->"; splice `:truncated`
        // before the closing ` -->` so the prefix-match in find-existing still works.
        let body = if payload.len() > 65_000 {
            payload.truncate(65_000);
            payload.push_str("\"]"); // best-effort close
            format!("<!-- ecp-impact-cache:V1:truncated -->\n{payload}")
        } else {
            format!("{CACHE_MARKER}\n{payload}")
        };

        // Try to find an existing marker comment to PATCH; otherwise POST a new one.
        let list_endpoint = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
        let jq_find = format!(".[] | select(.body | startswith(\"{CACHE_MARKER_PREFIX}\")) | .id");
        let list_out = Command::new("gh")
            .args(["api", &list_endpoint, "--jq", &jq_find])
            .output()
            .map_err(EcpError::Io)?;
        let existing_id = String::from_utf8_lossy(&list_out.stdout)
            .lines()
            .next()
            .map(str::to_owned)
            .filter(|s| !s.is_empty());

        // .output() captures gh's stdout (e.g. the new-comment URL from
        // `gh pr comment`) instead of letting it pass through to our parent
        // stdout — pr_analyze prints its JSON payload to stdout and the
        // workflow pipes that to /tmp/analysis.json, so any subprocess
        // output here would corrupt the JSON and break the downstream jq.
        let exec = if let Some(id) = existing_id {
            let patch_endpoint = format!("repos/{{owner}}/{{repo}}/issues/comments/{id}");
            Command::new("gh")
                .args(["api", "-X", "PATCH", &patch_endpoint, "-f"])
                .arg(format!("body={body}"))
                .output()
        } else {
            Command::new("gh")
                .args(["pr", "comment", &pr.to_string(), "--body", &body])
                .output()
        };
        let out = exec.map_err(EcpError::Io)?;
        if !out.status.success() {
            return Err(EcpError::GitDiff {
                reason: format!(
                    "gh write comment exit {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

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
        let empty: [std::path::PathBuf; 0] = [];
        assert_eq!(classify_area(&empty), None);
    }

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

    #[test]
    fn parse_impact_json_fixture() {
        let raw = include_str!("../../../tests/fixtures/pr_analyze/sample_impact.json");
        let parsed: ImpactJson = serde_json::from_str(raw).unwrap();
        // 2 directly changed symbols
        assert_eq!(parsed.changed_symbol_names(), vec!["FnA", "MethodB"]);
        // 3 unique callers at depth > 0: CallerC, CallerD, CallerE
        assert_eq!(parsed.impact_set_names().len(), 3);
        // file_path still parses (even though run() reads files from git
        // diff directly now — see ImpactJson note re: changed_files removal).
        assert_eq!(
            parsed.changed_symbols[0].file_path,
            "crates/ecp-cli/src/commands/impact.rs"
        );
    }

    use std::cell::RefCell;
    use std::collections::HashMap;

    struct MockGh {
        siblings: Vec<SiblingPr>,
        cached: HashMap<u32, Vec<String>>,
        writes: RefCell<Vec<(u32, Vec<String>)>>,
    }

    impl MockGh {
        fn new(siblings: Vec<SiblingPr>, cached: HashMap<u32, Vec<String>>) -> Self {
            Self {
                siblings,
                cached,
                writes: RefCell::new(Vec::new()),
            }
        }
    }

    impl GhClient for MockGh {
        fn list_sibling_prs(
            &self,
            _label: &str,
            _base_branch: &str,
            exclude: u32,
        ) -> Result<Vec<SiblingPr>, EcpError> {
            Ok(self
                .siblings
                .iter()
                .filter(|p| p.number != exclude)
                .cloned()
                .collect())
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
            vec![SiblingPr {
                number: 101,
                head_ref_oid: "abc".into(),
            }],
            cached,
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh,
            "merge-queue",
            "main",
            100,
            &["FnA".to_string(), "FnB".to_string()],
        )
        .unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn cross_pr_conflict_overlap() {
        let mut cached = HashMap::new();
        cached.insert(101, vec!["FnA".into(), "FnY".into()]);
        let gh = MockGh::new(
            vec![SiblingPr {
                number: 101,
                head_ref_oid: "abc".into(),
            }],
            cached,
        );
        let conflicts = detect_cross_pr_conflicts(
            &gh,
            "merge-queue",
            "main",
            100,
            &["FnA".to_string(), "FnB".to_string()],
        )
        .unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].pr, 101);
        assert_eq!(conflicts[0].overlap_symbols, vec!["FnA"]);
    }

    #[test]
    fn cross_pr_conflict_missing_cache_skipped() {
        // Sibling PR exists but has no cached impact yet (race on
        // near-simultaneous pushes). Should be SKIPPED — reporting it as a
        // conflict creates a deadlock where N parallel PRs all see N-1
        // uncached siblings and block themselves. Mergify's speculative
        // trial catches any real conflict at test time.
        let gh = MockGh::new(
            vec![SiblingPr {
                number: 101,
                head_ref_oid: "abc".into(),
            }],
            HashMap::new(),
        );
        let conflicts =
            detect_cross_pr_conflicts(&gh, "merge-queue", "main", 100, &["FnA".to_string()])
                .unwrap();
        assert!(conflicts.is_empty(), "uncached sibling should be skipped");
    }

    #[test]
    fn cross_pr_conflict_excludes_self() {
        // Sibling 101 has overlapping cached impact — should be reported.
        // Self (100) must not even appear in the candidate list.
        let mut cached = HashMap::new();
        cached.insert(101, vec!["FnA".into(), "FnZ".into()]);
        let gh = MockGh::new(
            vec![
                SiblingPr {
                    number: 100,
                    head_ref_oid: "self".into(),
                },
                SiblingPr {
                    number: 101,
                    head_ref_oid: "abc".into(),
                },
            ],
            cached,
        );
        let conflicts =
            detect_cross_pr_conflicts(&gh, "merge-queue", "main", 100, &["FnA".to_string()])
                .unwrap();
        // Only PR 101 should appear; self (100) filtered.
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].pr, 101);
    }
}
