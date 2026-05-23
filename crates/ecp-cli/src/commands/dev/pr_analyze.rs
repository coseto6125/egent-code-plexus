//! `ecp dev pr-analyze` — classify a PR by area / risk / cross-PR semantic
//! conflict, emit JSON consumed by `.github/workflows/ecp-pr-analyze.yml`
//! to apply labels + commit statuses for Mergify routing.
//!
//! Black-box wraps `ecp impact --baseline <ref> --format json` (subprocess),
//! so no tight coupling to impact's internal API.

use crate::output::OutputFormat;
use clap::Args;
use ecp_core::EcpError;
use serde::{Deserialize, Serialize};
use std::path::Path;

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

// ── impact subprocess types ──────────────────────────────────────────────────

/// One symbol that changed between baseline and HEAD.
/// Fields match the live `ecp impact --baseline --format json` shape.
#[derive(Deserialize, Debug)]
struct ChangedSymbol {
    pub name: String,
    /// `"Function"`, `"Method"`, `"Struct"`, `"Module"`, etc.
    #[allow(dead_code)]
    pub kind: String,
    /// Repo-relative path (forward slashes).
    #[serde(rename = "filePath")]
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
    /// Unique repo-relative file paths that contain at least one changed symbol.
    pub fn changed_files(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.changed_symbols
            .iter()
            .filter(|s| seen.insert(s.file_path.clone()))
            .map(|s| s.file_path.clone())
            .collect()
    }

    /// All symbol names reachable from changed symbols (depth > 0), deduplicated.
    /// Used to size the impact set for risk classification.
    pub fn impact_set_names(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        self.impact_by_symbol
            .iter()
            .flat_map(|entry| entry.impact.iter())
            .filter(|e| e.depth > 0 && seen.insert(e.name.clone()))
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
    let exe = std::env::current_exe()
        .map_err(|e| EcpError::InvalidArgument(format!("locate self exe: {e}")))?;
    let out = Command::new(&exe)
        .args(["impact", "--baseline", baseline, "--format", "json"])
        .output()
        .map_err(|e| EcpError::InvalidArgument(format!("spawn ecp impact: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::InvalidArgument(format!(
            "ecp impact failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| EcpError::InvalidArgument(format!("parse impact JSON: {e}")))
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

/// Risk bucket from impact set size. Thresholds 5/30 are dev-machine
/// measurements on this repo; revisit after a month of real usage.
pub fn classify_risk(impact_size: usize) -> Risk {
    match impact_size {
        0..=5 => Risk::Low,
        6..=30 => Risk::Medium,
        _ => Risk::High,
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
        assert_eq!(classify_area(&[]), None);
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
        // 1 unique file
        assert_eq!(
            parsed.changed_files()[0],
            "crates/ecp-cli/src/commands/impact.rs"
        );
    }
}
