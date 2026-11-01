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
//!
//! ## Folders of projects
//!
//! [`NestedRepoWatch`] refuses a tree with no git commit that holds two or
//! more repositories, before any file is parsed.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const ENV_OVERRIDE: &str = "ECP_INCLUDE_WORKTREES";

/// Nested repositories at which a tree with no git commit is refused.
const NESTED_REPOS_REFUSED_AT: usize = 2;

/// Watches a walk of a tree with no git commit for repositories inside it.
///
/// Two or more make the tree a folder of projects: that is where an agent's
/// failed `cd` lands, and indexing it parses every project at once (on
/// 2026-11-01 that took the host down). One is common, a non-git project
/// with one cloned dependency, and only earns a note. A tree with a commit is
/// not watched: its submodules and embedded clones belong to the project.
/// The build's archive extract of a dirty checkout has no `.git`, so it is
/// watched, but `git archive` writes no `.git` entry and it counts none.
/// `[index] allow_nested_repos = true` in the tree's `.ecp/config.toml`
/// turns the refusal off for a deliberate multi-repo workspace.
pub(crate) struct NestedRepoWatch {
    /// The walk root as the walk spells it, so walked paths strip against it.
    root: PathBuf,
    /// The root as the messages name it: `.` tells a lost agent nothing.
    shown_root: PathBuf,
    active: bool,
    /// Why `.ecp/config.toml` did not load, so a refusal does not ask for a
    /// setting the user already wrote.
    config_error: Option<String>,
    count: AtomicUsize,
    found: Mutex<Vec<PathBuf>>,
}

impl NestedRepoWatch {
    pub(crate) fn for_tree(root: &Path) -> Self {
        let config = ecp_core::config::load(root);
        let allowed = config
            .as_ref()
            .is_ok_and(|cfg| cfg.index.allow_nested_repos);
        Self {
            root: root.to_path_buf(),
            shown_root: dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            active: !allowed && !crate::git_cache::has_commit(root),
            config_error: config.err().map(|e| e.to_string()),
            count: AtomicUsize::new(0),
            found: Mutex::new(Vec::new()),
        }
    }

    /// Records `dir` when it is the top of a git checkout. The walk calls it
    /// for every directory it keeps.
    pub(crate) fn observe_dir(&self, dir: &Path) {
        if !self.active || dir == self.root {
            return;
        }
        // A repository's own `.git` tree holds hundreds of directories and no
        // checkout; skipping it by name saves a stat for each.
        let in_git_dir = dir
            .strip_prefix(&self.root)
            .is_ok_and(|rel| rel.components().any(|c| c.as_os_str() == ".git"));
        if !in_git_dir && crate::git_cache::is_repository_root(dir) {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.found
                .lock()
                .expect("nested-repo list is never held across a panic")
                .push(dir.to_path_buf());
        }
    }

    /// True once the walk has seen enough repositories to refuse, so it can
    /// stop instead of listing the rest of a machine-sized tree.
    pub(crate) fn refusal_reached(&self) -> bool {
        self.count.load(Ordering::Relaxed) >= NESTED_REPOS_REFUSED_AT
    }

    /// The verdict once the walk ends: an error from two repositories on, and
    /// for one a note that the caller prints where a note is wanted.
    pub(crate) fn finish(&self) -> io::Result<Option<String>> {
        let mut found = self
            .found
            .lock()
            .expect("nested-repo list is never held across a panic")
            .clone();
        found.sort();
        let names: Vec<String> = found
            .iter()
            .map(|dir| {
                dir.strip_prefix(&self.root)
                    .unwrap_or(dir)
                    .display()
                    .to_string()
            })
            .collect();
        let config_path = ecp_core::config::config_path(&self.shown_root);
        match names.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(format!(
                "note: {} has no git commit and holds the repository {only}; indexing the whole \
                 tree",
                self.shown_root.display()
            ))),
            several => Err(io::Error::other(format!(
                "refusing to index {}: it has no git commit and holds at least {} repositories \
                 ({}), so it is a folder of projects; run ecp inside one of them, query across \
                 them with `ecp group`, list them in {} to leave them out, or set \
                 `allow_nested_repos = true` under [index] in {} to index the whole tree{}",
                self.shown_root.display(),
                several.len(),
                several.join(", "),
                self.shown_root.join(crate::ECP_IGNORE_FILE).display(),
                config_path.display(),
                self.config_error
                    .as_deref()
                    .map(|e| format!(" (that config did not load: {e})"))
                    .unwrap_or_default()
            ))),
        }
    }
}

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
