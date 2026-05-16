//! Phase 1 — `build_l2` fast-path tests.
//!
//! L2 is commit-content-addressed (v2 layout, PR #55), so the fast path is
//! SHA-pure: same SHA + matching builder fingerprint → reuse the existing
//! commit_dir without re-running the analyzer pipeline. Dirty worktree
//! deltas live in the L1 session overlay (out of scope here).

use graph_nexus_cli::build::orchestrator;
use graph_nexus_core::registry::CommitBuildMeta;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// `build_l2` resolves the L2 root via `HOME`. Cargo runs integration tests
/// in parallel threads within the same binary, so every test that touches
/// `HOME` serialises through this guard. Poison from a panicking sibling
/// test is recoverable — we own no shared mutable state behind the lock.
static HOME_GUARD: Mutex<()> = Mutex::new(());

fn lock_home() -> std::sync::MutexGuard<'static, ()> {
    HOME_GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

fn init_repo_with_commit(worktree: &Path) {
    Command::new("git")
        .current_dir(worktree)
        .args(["init", "-q"])
        .status()
        .unwrap();
    std::fs::write(worktree.join("main.rs"), "fn main() {}\n").unwrap();
    git_commit(worktree, "init");
}

fn git_commit(worktree: &Path, msg: &str) {
    Command::new("git")
        .current_dir(worktree)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(worktree)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            msg,
        ])
        .status()
        .unwrap();
}

fn read_meta(commit_dir: &Path) -> CommitBuildMeta {
    CommitBuildMeta::read(&commit_dir.join("meta.json")).unwrap()
}

#[test]
fn fast_path_reuses_existing_l2_when_sha_matches() {
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    std::env::set_var("HOME", tmp.path().join("home"));

    let first = orchestrator::build_l2(&worktree, None).unwrap();
    let built_at_1 = read_meta(&first.commit_dir).built_at;

    // built_at uses RFC3339 with sub-second precision but sleep a beat to
    // be defensive against same-tick rebuilds slipping past assert_eq!.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let second = orchestrator::build_l2(&worktree, None).unwrap();
    let built_at_2 = read_meta(&second.commit_dir).built_at;

    assert_eq!(first.commit_dir, second.commit_dir);
    assert_eq!(
        built_at_1, built_at_2,
        "fast path must not re-run the analyzer pipeline on a hit"
    );
}

#[test]
fn fast_path_builds_new_l2_when_head_advances() {
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    std::env::set_var("HOME", tmp.path().join("home"));

    let first = orchestrator::build_l2(&worktree, None).unwrap();

    // HEAD-advancing commit → new SHA → different commit_dir.
    std::fs::write(worktree.join("main.rs"), "fn main() { let _ = 2; }\n").unwrap();
    git_commit(&worktree, "v2");

    let second = orchestrator::build_l2(&worktree, None).unwrap();

    assert_ne!(
        first.commit_dir, second.commit_dir,
        "new HEAD must produce a new L2 commit_dir"
    );
    assert_ne!(first.sha_hex, second.sha_hex);
}

#[test]
fn fast_path_rebuilds_on_fingerprint_mismatch() {
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    std::env::set_var("HOME", tmp.path().join("home"));

    let first = orchestrator::build_l2(&worktree, None).unwrap();
    let meta_path = first.commit_dir.join("meta.json");

    let mut stale = read_meta(&first.commit_dir);
    stale.builder_fingerprint = Some("stale-v0.0.0".to_string());
    let stale_built_at = stale.built_at.clone();
    CommitBuildMeta::write_atomic(&meta_path, &stale).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let _ = orchestrator::build_l2(&worktree, None).unwrap();
    let after = read_meta(&first.commit_dir);

    assert_ne!(
        after.builder_fingerprint.as_deref(),
        Some("stale-v0.0.0"),
        "fingerprint mismatch must trigger a rebuild that refreshes the field"
    );
    assert_ne!(
        after.built_at, stale_built_at,
        "rebuild must advance built_at"
    );
}

#[test]
fn fast_path_rebuilds_when_fingerprint_missing() {
    // Backward-compat: pre-fingerprint meta lacks the field. Treat as miss.
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    std::env::set_var("HOME", tmp.path().join("home"));

    let first = orchestrator::build_l2(&worktree, None).unwrap();
    let meta_path = first.commit_dir.join("meta.json");

    let mut legacy = read_meta(&first.commit_dir);
    legacy.builder_fingerprint = None;
    let legacy_built_at = legacy.built_at.clone();
    CommitBuildMeta::write_atomic(&meta_path, &legacy).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let _ = orchestrator::build_l2(&worktree, None).unwrap();
    let after = read_meta(&first.commit_dir);

    assert!(
        after.builder_fingerprint.is_some(),
        "rebuild must populate the fingerprint field"
    );
    assert_ne!(after.built_at, legacy_built_at);
}
