//! `filter_entry` policy shared by the canonical index path and the
//! `auto_ensure` staleness walk.
//!
//! Excludes directory subtrees that pollute the graph with isolated source
//! copies of the same files — the two main offenders:
//!
//! 1. **LLM-agent worktree containers** — `.claude/worktrees/<N>/` is the
//!    Claude Code convention; isolated copies of the repo with their own
//!    `.git` file. Indexing them duplicates every symbol N+1 times and
//!    confuses cross-file resolution.
//!
//! 2. **Nested git worktrees** — any descendant directory with a `.git`
//!    FILE (not dir) is a `git worktree add` sibling pointing at a
//!    different commit. Including it in the parent's index falsely binds
//!    the parent graph to the sibling's symbols.
//!
//! Always allows the indexed root itself (the caller selected it on purpose).
//!
//! ## Escape hatch
//!
//! Set `ECP_INCLUDE_WORKTREES=1` to disable both filters — useful when the
//! user genuinely wants to index a worktree-containing dir or wants to
//! verify cross-worktree symbol overlap.

use std::path::{Component, Path};

const ENV_OVERRIDE: &str = "ECP_INCLUDE_WORKTREES";

/// Returns `true` when `entry_path` should be pruned from the walk.
///
/// `root` is the canonical index root — entries with that exact path are
/// always allowed (the user picked it). Anything below is subject to the
/// two checks documented at the module level.
pub fn is_skippable_worktree_descendant(entry_path: &Path, root: &Path) -> bool {
    if std::env::var(ENV_OVERRIDE).is_ok() {
        return false;
    }

    // Root itself is never skipped.
    let Ok(rel) = entry_path.strip_prefix(root) else {
        return false;
    };
    if rel.as_os_str().is_empty() {
        return false;
    }

    // L2 — hard-coded `.claude/worktrees/<n>` segment pair. Zero I/O.
    let normal: Vec<&std::ffi::OsStr> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    for w in normal.windows(2) {
        if w[0] == ".claude" && w[1] == "worktrees" {
            return true;
        }
    }

    // L1 — descendant dir carrying a `.git` FILE (worktree marker). Costs
    // one `symlink_metadata` per descended dir, which the walker is about
    // to descend into anyway; the syscall cost is amortised by also
    // pruning the entire subtree on hit.
    if let Ok(meta) = std::fs::symlink_metadata(entry_path.join(".git")) {
        if meta.is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // `env_override_disables_all_skips` mutates the process-global
    // `ECP_INCLUDE_WORKTREES` env var; every other test reads it via
    // `is_skippable_worktree_descendant`. Cargo runs unit tests in parallel
    // within a binary, so without serialisation the writer flips the var
    // mid-flight and the reader tests observe the override unexpectedly →
    // intermittent `assertion failed` panic on the descendant tests. The
    // mutex holds for the entire test body so the env-var state is well-
    // defined throughout each assertion.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // Poison-tolerant lock helper: a panic in a preceding test would poison
    // the Mutex, propagating spurious failures to every later test. We only
    // need mutual exclusion, not the consistency contract poisoning
    // enforces, so unwrap-or-inner is correct here.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn root_itself_is_allowed() {
        let _guard = lock();
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_skippable_worktree_descendant(tmp.path(), tmp.path()));
    }

    #[test]
    fn plain_descendant_is_allowed() {
        let _guard = lock();
        let tmp = tempfile::tempdir().unwrap();
        let child = tmp.path().join("src");
        fs::create_dir(&child).unwrap();
        assert!(!is_skippable_worktree_descendant(&child, tmp.path()));
    }

    #[test]
    fn claude_worktrees_segment_is_skipped() {
        let _guard = lock();
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join(".claude/worktrees/feature-x");
        fs::create_dir_all(&wt).unwrap();
        assert!(is_skippable_worktree_descendant(&wt, tmp.path()));
        // Files inside also skipped.
        let f = wt.join("inner.rs");
        fs::write(&f, "").unwrap();
        assert!(is_skippable_worktree_descendant(&f, tmp.path()));
    }

    #[test]
    fn nested_git_file_marks_worktree() {
        let _guard = lock();
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("sibling-worktree");
        fs::create_dir(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(is_skippable_worktree_descendant(&wt, tmp.path()));
    }

    #[test]
    fn nested_git_directory_is_not_marked() {
        let _guard = lock();
        // `.git` as a directory = primary worktree's own metadata (already
        // ignored by `WalkBuilder::hidden`/`SKIP_DIRS`). filter_entry should
        // not double-fire on it.
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        fs::create_dir(&inner).unwrap();
        fs::create_dir(inner.join(".git")).unwrap();
        assert!(!is_skippable_worktree_descendant(&inner, tmp.path()));
    }

    #[test]
    fn env_override_disables_all_skips() {
        let _guard = lock();
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join(".claude/worktrees/feature-x");
        fs::create_dir_all(&wt).unwrap();
        std::env::set_var(ENV_OVERRIDE, "1");
        assert!(!is_skippable_worktree_descendant(&wt, tmp.path()));
        std::env::remove_var(ENV_OVERRIDE);
    }
}
