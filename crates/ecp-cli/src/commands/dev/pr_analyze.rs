//! `ecp dev pr-analyze` — classify a PR by area / risk / cross-PR semantic
//! conflict, emit JSON consumed by `.github/workflows/ecp-pr-analyze.yml`
//! to apply labels + commit statuses for Mergify routing.
//!
//! Black-box wraps `ecp impact --baseline <ref> --format json` (subprocess),
//! so no tight coupling to impact's internal API.

use crate::output::OutputFormat;
use clap::Args;
use ecp_core::EcpError;
use serde::Serialize;
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
}
