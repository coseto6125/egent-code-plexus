//! Graph coverage at its own SHA — catches partial builds that every
//! sha/mtime freshness probe calls fresh.
//!
//! Observed in the wild: a 0.39s build indexed 1 of 4 workspace crates, then
//! answered `found:false, status:success` for symbols in the other three for
//! 15 days. Staleness checks can't see it (the sidecar SHA matches); only a
//! file-population comparison can. The extension universe is taken from the
//! graph's own File nodes, so languages ecp doesn't parse never count as
//! missing.

use crate::commands::admin::doctor::CheckResult;
use crate::commands::admin::index::{self, IndexArgs};
use crate::graph_path;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Both gates must trip: a handful of unindexed files is normal churn in a
/// small repo, and a fixed count alone would flag 10-of-50k on a monorepo.
const MIN_MISSING_FILES: usize = 10;
const MAX_MISSING_RATIO: f64 = 0.30;

pub(crate) fn check(fix: bool) -> CheckResult {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("coverage", format!("cannot read cwd: {e}")),
    };
    check_in(&cwd, fix)
}

fn check_in(cwd: &Path, fix: bool) -> CheckResult {
    let resolved = graph_path::resolve(&PathBuf::from(".ecp/graph.bin"), cwd);
    // HEAD may have moved past the last build (resolve is sha-keyed) — audit
    // the latest *published* graph instead, since that is what queries
    // warm-attach and serve.
    let graph = if resolved.is_file() {
        resolved
    } else {
        match latest_published_graph(cwd) {
            Some(p) if p.is_file() => p,
            _ => {
                return CheckResult::ok("coverage", "skipped — no loadable graph (see index check)")
            }
        }
    };
    let Ok(engine) = crate::engine::Engine::load(&graph) else {
        return CheckResult::ok("coverage", "skipped — no loadable graph (see index check)");
    };
    let Ok(g) = engine.graph() else {
        return CheckResult::ok("coverage", "skipped — no loadable graph (see index check)");
    };

    let indexed: HashSet<String> = g
        .files
        .iter()
        .map(|f| f.path.resolve(&g.string_pool).to_string())
        .collect();
    let known_exts: HashSet<&str> = indexed.iter().filter_map(|p| ext_of(p)).collect();

    let Some(tracked) = git_tracked_files(cwd) else {
        return CheckResult::ok("coverage", "skipped — not a git repository");
    };

    let candidates: Vec<&String> = tracked
        .iter()
        .filter(|p| ext_of(p).is_some_and(|e| known_exts.contains(e)))
        .collect();
    let missing: Vec<&str> = candidates
        .iter()
        .filter(|p| !indexed.contains(p.as_str()))
        .map(|p| p.as_str())
        .collect();

    if candidates.is_empty() {
        return CheckResult::ok("coverage", "no tracked files with graph-known extensions");
    }
    let ratio = missing.len() as f64 / candidates.len() as f64;
    if missing.len() < MIN_MISSING_FILES || ratio <= MAX_MISSING_RATIO {
        return CheckResult::ok(
            "coverage",
            format!(
                "graph covers {}/{} tracked source files",
                candidates.len() - missing.len(),
                candidates.len()
            ),
        );
    }

    let sample: Vec<&str> = missing.iter().take(3).copied().collect();
    let mut result = CheckResult::fail(
        "coverage",
        format!(
            "graph is missing {} of {} tracked source files with graph-known \
             extensions ({:.0}%) — e.g. {}; the graph was likely built from a \
             partial tree",
            missing.len(),
            candidates.len(),
            ratio * 100.0,
            sample.join(", ")
        ),
    )
    .with_remediation("ecp admin index --force --repo .");

    if fix {
        // Same-SHA partial graphs look fresh to the non-force path, so the
        // remediation must force.
        let args = IndexArgs {
            repo: cwd.to_string_lossy().into_owned(),
            force: true,
            dump_resolver: None,
            quiet: true,
        };
        result.fix_applied = Some(index::run(args).is_ok());
    }
    result
}

fn ext_of(path: &str) -> Option<&str> {
    Path::new(path).extension().and_then(|e| e.to_str())
}

/// Newest published commit dir's graph, mirroring `graph_path::resolve_v2`'s
/// registry layout walk but by mtime instead of HEAD SHA.
fn latest_published_graph(cwd: &Path) -> Option<PathBuf> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(cwd).ok()?;
    let commits = home_ecp.join(&repo_dir_name).join("commits");
    crate::commit_lookup::find_latest_by_mtime(&commits).map(|dir| dir.join("graph.bin"))
}

/// Tracked files relative to the repo top (`--full-name` keeps subdir cwds
/// consistent with the repo-relative paths the graph stores). `None` when not
/// a git repo.
fn git_tracked_files(cwd: &Path) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(["ls-files", "--full-name", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}
