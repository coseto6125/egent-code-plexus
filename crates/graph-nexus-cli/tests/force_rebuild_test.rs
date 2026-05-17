use graph_nexus_cli::build::force::invalidate_matching_l1;
use graph_nexus_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::fs;
use std::path::Path;

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
