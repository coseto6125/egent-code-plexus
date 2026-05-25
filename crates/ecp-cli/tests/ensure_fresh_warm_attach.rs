//! FU-2026-05-23-004 integration tests: warm-attach fallback for OOB branch switch.
//!
//! When a user runs `git checkout` from an IDE/GUI (no PostToolUse hook fires),
//! the next `ecp` invocation hits `ensure_fresh` with a Missing graph for the
//! new HEAD SHA. Rather than blocking 1-2s on a cold rebuild, `ensure_fresh`
//! attaches the most recently published sibling SHA's graph and fires a
//! background rebuild. Tests here verify:
//!
//! 1. One published SHA → warm-attach picks it up, WARM_ATTACH_COUNT increments.
//! 2. After the background rebuild completes, the next ensure_fresh hits Ready
//!    directly (no warm-attach needed).
//! 3. No sibling SHAs at all → falls back to synchronous build_l2 (no regression).
//! 4. `is_stale_for_sha` flag propagates through `Engine::load_warm`.
//!
//! Tests that mutate process-global HOME / ECP_HOME are serialised via ENV_LOCK
//! to prevent interference with other test binaries that share these statics.

use ecp_cli::auto_ensure::{self, test_counters, EnsureFreshOutcome};
use ecp_cli::engine::Engine;
use ecp_cli::graph_path;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct EnvSnapshot {
    home: Option<std::ffi::OsString>,
    ecp_home: Option<std::ffi::OsString>,
    skip_bg: Option<std::ffi::OsString>,
}

impl EnvSnapshot {
    fn take() -> Self {
        Self {
            home: std::env::var_os("HOME"),
            ecp_home: std::env::var_os("ECP_HOME"),
            skip_bg: std::env::var_os("ECP_SKIP_BG_REBUILD"),
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
        match &self.skip_bg {
            Some(v) => std::env::set_var("ECP_SKIP_BG_REBUILD", v),
            None => std::env::remove_var("ECP_SKIP_BG_REBUILD"),
        }
    }
}

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn git_init_with_commit(p: &Path) -> String {
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
    let out = g(&["rev-parse", "HEAD"]);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Advance the repo to a new SHA (new commit) without yet building a graph for it.
/// Simulates an OOB `git checkout` to a new branch where no index exists.
fn add_second_commit(p: &Path) -> String {
    let g = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    };
    fs::write(p.join("lib.rs"), "pub fn updated_fn() {}\n").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "second"]);
    let out = g(&["rev-parse", "HEAD"]);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Build the index for `repo` using `ECP_HOME=cache_dir`.
fn run_admin_index(repo: &Path, cache_dir: &Path) {
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", cache_dir)
        .env_remove("ECP_HOME")
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── Test 1 ────────────────────────────────────────────────────────────────────

/// One published SHA → warm-attach picks it up; WARM_ATTACH_COUNT increments;
/// returned outcome is WarmAttach with a valid path.
#[test]
fn warm_attach_picks_up_sibling_sha_when_missing() {
    let _env_guard = lock_env();
    let _snap = EnvSnapshot::take();

    let repo_tmp = TempDir::new().unwrap();
    let cache_tmp = TempDir::new().unwrap();
    let repo = repo_tmp.path();
    let cache = cache_tmp.path();

    // Build an index for the initial commit (SHA-1).
    git_init_with_commit(repo);
    run_admin_index(repo, cache);

    // Advance the repo to SHA-2 WITHOUT building a graph for it.
    // This simulates an OOB `git checkout -b new-branch` followed by a commit.
    add_second_commit(repo);

    std::env::set_var("HOME", cache);
    std::env::remove_var("ECP_HOME");
    // Suppress the detached rebuild: we assert the WarmAttach outcome + counter,
    // not the rebuild itself, and a leaked `sh` child fails nextest on Windows.
    std::env::set_var("ECP_SKIP_BG_REBUILD", "1");
    test_counters::reset();

    // Resolve via the same logic as main.rs (cli.graph default is the relative
    // sentinel `.ecp/graph.bin`). For SHA-2 (no published graph), resolve_v2
    // returns None → sentinel unchanged → ensure_index reports Missing →
    // warm-attach fires with SHA-1's graph as the sibling.
    let legacy_sentinel = std::path::Path::new(".ecp/graph.bin");
    let resolved = graph_path::resolve(legacy_sentinel, repo);
    let outcome = auto_ensure::ensure_fresh(&resolved, repo)
        .expect("ensure_fresh should succeed with warm-attach");

    assert!(
        matches!(outcome, EnsureFreshOutcome::WarmAttach { .. }),
        "expected WarmAttach, got {:?}",
        outcome
    );
    assert_eq!(
        test_counters::warm_attach_calls(),
        1,
        "WARM_ATTACH_COUNT must be 1 after warm-attach"
    );

    if let EnsureFreshOutcome::WarmAttach { sibling_graph_path } = outcome {
        assert!(
            sibling_graph_path.is_file(),
            "sibling_graph_path must exist"
        );
        let eng = Engine::load_warm(&sibling_graph_path)
            .expect("load_warm of sibling graph must succeed");
        assert!(
            eng.is_stale_for_sha,
            "engine loaded via load_warm must report is_stale_for_sha=true"
        );
    }
}

// ── Test 2 ────────────────────────────────────────────────────────────────────

/// After the background rebuild completes, the next ensure_fresh resolves to
/// Ready (no warm-attach). This verifies there is no regression in the normal
/// fresh-graph path after the new SHA is indexed.
#[test]
fn after_rebuild_ensure_fresh_returns_ready() {
    let _env_guard = lock_env();
    let _snap = EnvSnapshot::take();

    let repo_tmp = TempDir::new().unwrap();
    let cache_tmp = TempDir::new().unwrap();
    let repo = repo_tmp.path();
    let cache = cache_tmp.path();

    // Build index for SHA-1.
    git_init_with_commit(repo);
    run_admin_index(repo, cache);

    // Advance to SHA-2 and build its index directly (simulating completed rebuild).
    add_second_commit(repo);
    run_admin_index(repo, cache);

    std::env::set_var("HOME", cache);
    std::env::remove_var("ECP_HOME");
    test_counters::reset();

    // Resolve as main.rs does. SHA-2 is now indexed → resolve_v2 returns the
    // real commits/<dir>/graph.bin → ensure_index sees the file → Ready.
    let legacy_sentinel = std::path::Path::new(".ecp/graph.bin");
    let resolved = graph_path::resolve(legacy_sentinel, repo);
    let outcome = auto_ensure::ensure_fresh(&resolved, repo)
        .expect("ensure_fresh should return Ready after rebuild");

    assert!(
        matches!(outcome, EnsureFreshOutcome::Ready),
        "expected Ready after rebuild completes, got {:?}",
        outcome
    );
    assert_eq!(
        test_counters::warm_attach_calls(),
        0,
        "WARM_ATTACH_COUNT must be 0 when graph is already built for current SHA"
    );
}

// ── Test 3 ────────────────────────────────────────────────────────────────────

/// No sibling SHAs exist (brand-new repo, nothing indexed yet) → falls back to
/// synchronous build_l2. No warm-attach; no regression in fresh-start behavior.
#[test]
fn no_sibling_falls_back_to_sync_build() {
    let _env_guard = lock_env();
    let _snap = EnvSnapshot::take();

    let repo_tmp = TempDir::new().unwrap();
    let cache_tmp = TempDir::new().unwrap();
    let repo = repo_tmp.path();
    let cache = cache_tmp.path();

    git_init_with_commit(repo);

    std::env::set_var("HOME", cache);
    std::env::remove_var("ECP_HOME");
    test_counters::reset();

    // No admin index run → commits/ dir absent → resolve_v2 returns None →
    // sentinel fallback → ensure_index reports Missing → no sibling graph →
    // sync build_l2 fires.
    let legacy_sentinel = std::path::Path::new(".ecp/graph.bin");
    let resolved = graph_path::resolve(legacy_sentinel, repo);
    let outcome = auto_ensure::ensure_fresh(&resolved, repo)
        .expect("ensure_fresh should succeed via sync build_l2");

    assert!(
        matches!(outcome, EnsureFreshOutcome::Ready),
        "expected Ready (sync build), got {:?}",
        outcome
    );
    assert_eq!(
        test_counters::warm_attach_calls(),
        0,
        "WARM_ATTACH_COUNT must be 0 when no sibling exists"
    );
}

/// A first Missing probe in a long-lived process can happen before any sibling
/// graph exists, forcing a sync build. That negative lookup must not poison the
/// process cache; after the sync build publishes SHA-1, a later SHA-2 should
/// warm-attach to it.
#[test]
fn no_sibling_lookup_does_not_poison_later_warm_attach() {
    let _env_guard = lock_env();
    let _snap = EnvSnapshot::take();

    let repo_tmp = TempDir::new().unwrap();
    let cache_tmp = TempDir::new().unwrap();
    let repo = repo_tmp.path();
    let cache = cache_tmp.path();

    git_init_with_commit(repo);

    std::env::set_var("HOME", cache);
    std::env::remove_var("ECP_HOME");
    // The later warm-attach spawns a detached rebuild; suppress it so nextest
    // does not flag a leaked `sh` child on Windows.
    std::env::set_var("ECP_SKIP_BG_REBUILD", "1");
    test_counters::reset();

    let legacy_sentinel = std::path::Path::new(".ecp/graph.bin");
    let first_resolved = graph_path::resolve(legacy_sentinel, repo);
    let first = auto_ensure::ensure_fresh(&first_resolved, repo)
        .expect("first ensure_fresh should sync-build without sibling");
    assert!(
        matches!(first, EnsureFreshOutcome::Ready),
        "expected initial sync build, got {:?}",
        first
    );
    assert_eq!(
        test_counters::warm_attach_calls(),
        0,
        "initial no-sibling path must not warm-attach"
    );

    add_second_commit(repo);

    let second_resolved = graph_path::resolve(legacy_sentinel, repo);
    let second = auto_ensure::ensure_fresh(&second_resolved, repo)
        .expect("second ensure_fresh should warm-attach to the first build");
    assert!(
        matches!(second, EnsureFreshOutcome::WarmAttach { .. }),
        "expected later warm-attach after sync build published a sibling, got {:?}",
        second
    );
    assert_eq!(
        test_counters::warm_attach_calls(),
        1,
        "later missing SHA must warm-attach despite the earlier no-sibling lookup"
    );
}

// ── Test 4 ────────────────────────────────────────────────────────────────────

/// `is_stale_for_sha` flag propagates through `Engine::load_warm` and is false
/// for engines created via the normal `Engine::load` path. This test is
/// self-contained (no process-global env mutation) — it directly constructs
/// the commits dir path from the known cache layout.
#[test]
fn stale_flag_propagates_through_load_warm() {
    let repo_tmp = TempDir::new().unwrap();
    let cache_tmp = TempDir::new().unwrap();
    let repo = repo_tmp.path();
    let cache = cache_tmp.path();

    git_init_with_commit(repo);
    run_admin_index(repo, cache);

    // Cache layout: `<cache>/.ecp/<repo_dir>/commits/<sha_dir>/graph.bin`.
    let ecp_home = cache.join(".ecp");
    let commits_dir = fs::read_dir(&ecp_home)
        .expect("ecp home must exist after admin index")
        .filter_map(Result::ok)
        .find(|e| e.path().join("commits").is_dir())
        .expect("at least one repo dir after admin index")
        .path()
        .join("commits");
    let graph_bin = ecp_cli::commit_lookup::find_latest_by_mtime(&commits_dir)
        .expect("at least one commit dir after admin index")
        .join("graph.bin");

    let normal_eng = Engine::load(&graph_bin).expect("Engine::load must succeed");
    assert!(
        !normal_eng.is_stale_for_sha,
        "Engine::load must set is_stale_for_sha=false"
    );

    let warm_eng = Engine::load_warm(&graph_bin).expect("Engine::load_warm must succeed");
    assert!(
        warm_eng.is_stale_for_sha,
        "Engine::load_warm must set is_stale_for_sha=true"
    );
}
