//! Graph index freshness, via the same staleness logic agent commands use.

use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::commands::admin::doctor::CheckResult;
use crate::commands::admin::index::{self, IndexArgs};
use crate::graph_path;
use std::path::PathBuf;

pub(crate) fn check(fix: bool) -> CheckResult {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("index", format!("cannot read cwd: {e}")),
    };
    check_in(&cwd, fix)
}

fn check_in(cwd: &std::path::Path, fix: bool) -> CheckResult {
    let graph = graph_path::resolve(&PathBuf::from(".ecp/graph.bin"), cwd);

    let remediation = "ecp admin index --repo .";
    let mut result = match ensure_index(&graph, cwd) {
        Ok(EnsureResult::Ready) => return CheckResult::ok("index", "graph is fresh"),
        Ok(EnsureResult::Stale { age_seconds, .. }) => CheckResult::warn(
            "index",
            format!("stale — graph built {age_seconds}s before latest source change"),
        )
        .with_remediation(remediation),
        Ok(EnsureResult::Missing) => {
            CheckResult::fail("index", "no graph index found").with_remediation(remediation)
        }
        Err(e) => return CheckResult::fail("index", format!("freshness probe failed: {e}")),
    };

    if fix {
        // Refuse to auto-index a non-git cwd: `index --repo .` would fall back
        // to scanning the whole subtree as an ad-hoc source tree (no file-count
        // cap → OOM in $HOME / large mounts). Downgrade to a warn that names
        // the manual escape hatch.
        if !is_git_repo(cwd) {
            return CheckResult::warn(
                "index",
                "not a git repository — skipping index fix (run `ecp admin index --repo <path>` \
                 explicitly to index a non-git tree)",
            );
        }
        let args = IndexArgs {
            repo: cwd.to_string_lossy().into_owned(),
            force: false,
            dump_resolver: None,
            quiet: true,
        };
        result.fix_applied = Some(index::run(args).is_ok());
    }
    result
}

/// True when `cwd` is inside a git repository. The `--fix` index remediation
/// is gated on this: `ecp admin index --repo .` falls back to scanning the
/// entire cwd subtree as an ad-hoc source tree when there is no git HEAD
/// (orchestrator::head_sha_hex synthesizes a path-bound SHA). Auto-triggering
/// that from `doctor --fix` in a large non-git dir ($HOME, /, a mount of
/// media/node_modules) reads every recognized file into one in-memory graph
/// with no file-count cap → OOM. We refuse the fix there and report instead.
fn is_git_repo(cwd: &std::path::Path) -> bool {
    crate::git_cache::common_dir(cwd).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{check_in, is_git_repo};
    use crate::commands::admin::doctor::CheckStatus;

    #[test]
    fn non_git_dir_is_not_indexable() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            !is_git_repo(tmp.path()),
            "a fresh tempdir is not a git repo — fix must not auto-index it"
        );
    }

    #[test]
    fn git_dir_is_indexable() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(
            is_git_repo(tmp.path()),
            "a git-initialized dir must remain eligible for the index fix"
        );
    }

    /// The OOM regression: `--fix` in a non-git dir must NOT run the indexer
    /// (which would scan the whole subtree). It downgrades to a warn and never
    /// sets `fix_applied`.
    #[test]
    fn fix_in_non_git_dir_skips_indexing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check_in(tmp.path(), true);
        assert_eq!(
            result.status,
            CheckStatus::Warn,
            "non-git --fix must warn, not attempt an index build"
        );
        assert!(
            result.fix_applied.is_none(),
            "non-git --fix must not run the indexer, so fix_applied stays unset"
        );
        assert!(
            result.message.contains("not a git repository"),
            "warn message must explain why the fix was skipped, got: {}",
            result.message
        );
    }
}
