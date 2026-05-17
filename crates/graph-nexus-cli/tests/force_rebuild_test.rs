use graph_nexus_cli::build::force::{force_rebuild_l2, invalidate_matching_l1};
use graph_nexus_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// Serialize tests that mutate `HOME`. Tests using set_var("HOME", ...) MUST
/// hold this lock for the duration of the test.
static HOME_LOCK: Mutex<()> = Mutex::new(());

const SHA: &str = "abc123def456789012345678901234567890abcd";
const SHA2: &str = "11ee22dd33cc44bb55aa66998877665544332211";
const DIRNAME: &str = "branch_main__abc123def456789012345678901234567890abcd";

fn setup_repo_with_l2(tmp: &Path) {
    let commits = tmp.join("commits").join(DIRNAME);
    fs::create_dir_all(&commits).unwrap();
    let cm = graph_nexus_core::registry::CommitBuildMeta {
        version: 1,
        sha: SHA.into(),
        source_type: graph_nexus_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 0,
        embedding_status: graph_nexus_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
    };
    graph_nexus_core::registry::CommitBuildMeta::write_atomic(&commits.join("meta.json"), &cm)
        .unwrap();
}

fn add_session(tmp: &Path, sid: &str, base_sha: &str, with_dirty: bool) {
    let sd = tmp.join("sessions").join(sid);
    fs::create_dir_all(sd.join("graph_overlay")).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: base_sha.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    let df = if with_dirty {
        let mut d = DirtyFiles::empty();
        d.entries.insert(
            "src/a.rs".into(),
            DirtyEntry {
                mtime_ns: 1,
                content_hash: "x".into(),
                fragment_id: "frag1".into(),
                tantivy_delta_segment: None,
                parse_failed: false,
                dirty_symbols: vec![],
            },
        );
        d
    } else {
        DirtyFiles::empty()
    };
    DirtyFiles::write_atomic(&sd.join("dirty_files.json"), &df).unwrap();
}

#[test]
fn invalidate_keeps_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_clean", SHA, false);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 1);
    assert_eq!(report.invalidated, 0);
    assert!(tmp.path().join("sessions/sid_clean").exists());
}

#[test]
fn invalidate_renames_augmented_to_stale() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_dirty", SHA, true);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 1);
    assert!(!tmp.path().join("sessions/sid_dirty").exists());
    let sha8 = &SHA[..8];
    let stale = tmp.path().join(format!("sessions/sid_dirty.stale-{sha8}"));
    assert!(stale.exists());
}

#[test]
fn invalidate_ignores_sessions_for_other_sha() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_other", SHA2, true);
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 0);
    assert!(tmp.path().join("sessions/sid_other").exists());
}

#[test]
fn invalidate_skips_already_stale_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    fs::create_dir_all(
        tmp.path()
            .join(format!("sessions/sid_zombie.stale-{}", &SHA[..8])),
    )
    .unwrap();
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.kept, 0);
    assert_eq!(report.invalidated, 0);
}

fn git_init(p: &Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["init", "-q"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.email", "t@t"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.name", "t"])
        .status()
        .unwrap();
    fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["commit", "-qm", "init"])
        .status()
        .unwrap();
    let o = Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn force_rebuild_l2_when_l2_absent_builds_fresh() {
    let _g = HOME_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", home.path());

    let r = force_rebuild_l2(wt.path(), &sha).unwrap();
    assert_eq!(r.sha_hex, sha);
    assert!(r.rebuilt);
    assert!(r.commit_dir.join("graph.bin").exists());
    assert!(r.commit_dir.join("meta.json").exists());
}

#[test]
fn force_rebuild_l2_drops_existing_dir_and_rebuilds() {
    let _g = HOME_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", home.path());

    let initial = graph_nexus_cli::build::orchestrator::build_l2(wt.path(), None).unwrap();
    let first_mtime = fs::metadata(initial.commit_dir.join("graph.bin"))
        .unwrap()
        .modified()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let r = force_rebuild_l2(wt.path(), &sha).unwrap();
    let second_mtime = fs::metadata(r.commit_dir.join("graph.bin"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        second_mtime > first_mtime,
        "graph.bin should have newer mtime after force rebuild"
    );
}

#[test]
fn force_rebuild_l2_invalidates_dirty_session_with_same_base_sha() {
    let _g = HOME_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", home.path());

    let initial = graph_nexus_cli::build::orchestrator::build_l2(wt.path(), None).unwrap();
    let repo_root = initial.commit_dir.parent().unwrap().parent().unwrap().to_path_buf();
    add_session(&repo_root, "sid_dirty", &sha, true);
    add_session(&repo_root, "sid_clean", &sha, false);

    force_rebuild_l2(wt.path(), &sha).unwrap();

    assert!(
        !repo_root.join("sessions/sid_dirty").exists(),
        "dirty session should be renamed .stale-*"
    );
    assert!(
        repo_root.join("sessions/sid_clean").exists(),
        "clean session should be kept"
    );
}

#[test]
fn invalidate_classifies_corrupt_session_as_stale_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo_with_l2(tmp.path());
    add_session(tmp.path(), "sid_corrupt", SHA, false);
    fs::write(
        tmp.path().join("sessions/sid_corrupt/dirty_files.json"),
        b"{ broken",
    )
    .unwrap();
    let report = invalidate_matching_l1(tmp.path(), SHA).unwrap();
    assert_eq!(report.stale_skipped, 1);
    assert!(tmp.path().join("sessions/sid_corrupt").exists());
}
