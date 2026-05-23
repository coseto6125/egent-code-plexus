//! FU-2026-05-23-047 regression: when the resolved ECP cache root lives
//! INSIDE the current worktree, `ensure_fresh` must synchronously join the
//! background tantivy writer before returning. Without the drain, the
//! `.ecp/<repo>/commits/.../tantivy/` directory keeps growing while
//! downstream `git stash push -u` (run by `ecp review --verdicts`) tries
//! to enumerate-then-remove it, producing the intermittent
//! `failed to remove ...tantivy: Directory not empty` failure that was
//! observed on `review_verdicts_test.rs` + `review_verdicts_indirect_dispatch_test.rs`
//! under full-suite parallel `cargo test` runs.
//!
//! Lives in its own test binary so `test_counters` reset can't be raced by
//! other test files that touch the same statics (`incremental_wired.rs`).

use ecp_cli::auto_ensure::{self, test_counters};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

/// Serialise tests that mutate process-global HOME / ECP_HOME and read
/// `test_counters`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn git_init_with_commit(p: &Path) {
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    fs::write(p.join("lib.rs"), "pub fn original_fn() {}\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "init"]);
}

/// RAII guard restoring HOME / ECP_HOME on Drop, even on test panic.
struct EnvSnapshot {
    home: Option<std::ffi::OsString>,
    ecp_home: Option<std::ffi::OsString>,
}

impl EnvSnapshot {
    fn take() -> Self {
        Self {
            home: std::env::var_os("HOME"),
            ecp_home: std::env::var_os("ECP_HOME"),
        }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        match &self.home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.ecp_home {
            Some(v) => std::env::set_var("ECP_HOME", v),
            None => std::env::remove_var("ECP_HOME"),
        }
    }
}

#[test]
fn ensure_fresh_drains_tantivy_when_ecp_cache_inside_worktree() {
    let _env_guard = lock_env();
    let _snapshot = EnvSnapshot::take();

    let tmp = TempDir::new().expect("tempdir");
    let worktree = tmp.path();
    git_init_with_commit(worktree);

    // Point HOME at the worktree so resolve_home_ecp() returns
    // `<worktree>/.ecp/` — the exact setup the two flaky review-verdicts
    // tests use, and the trigger condition for the race.
    std::env::set_var("HOME", worktree);
    std::env::remove_var("ECP_HOME");

    test_counters::reset();

    // graph_path is missing → ensure_fresh takes the `Missing` arm → build_l2
    // spawns the tantivy thread → drain_tantivy_if_inside_worktree must fire.
    let graph_path = worktree.join(".ecp").join("graph.bin");
    let result = auto_ensure::ensure_fresh(&graph_path, worktree);
    assert!(
        result.is_ok(),
        "ensure_fresh failed in nested-HOME fixture: {:?}",
        result.err()
    );

    assert_eq!(
        test_counters::tantivy_join_calls(),
        1,
        "ensure_fresh must call join_background exactly once when the ECP \
         cache root resolves inside the worktree (HOME={}); otherwise the \
         background tantivy writer races with downstream git stash / status",
        worktree.display(),
    );
}

#[test]
fn ensure_fresh_skips_tantivy_drain_when_cache_outside_worktree() {
    let _env_guard = lock_env();
    let _snapshot = EnvSnapshot::take();

    let worktree_tmp = TempDir::new().expect("worktree tempdir");
    let cache_tmp = TempDir::new().expect("cache tempdir");
    let worktree = worktree_tmp.path();
    git_init_with_commit(worktree);

    // Production-shaped layout: HOME (and thus ~/.ecp/) lives elsewhere.
    std::env::set_var("HOME", cache_tmp.path());
    std::env::remove_var("ECP_HOME");

    test_counters::reset();

    let graph_path = worktree.join(".ecp").join("graph.bin");
    let _ = auto_ensure::ensure_fresh(&graph_path, worktree);

    assert_eq!(
        test_counters::tantivy_join_calls(),
        0,
        "ensure_fresh must NOT pay the join cost when the ECP cache root \
         is outside the worktree — that's the prod fast path and the whole \
         point of CI-B's background spawn",
    );
}
