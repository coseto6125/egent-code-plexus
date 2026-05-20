use ecp_cli::session::state::classify;
use ecp_core::session::{DirtyEntry, DirtyFiles, SessionMeta, SessionState, StaleReason};
use std::fs;
use std::path::Path;

fn setup_repo(tmp: &Path, sha: &str, dirname: &str) {
    let commits = tmp.join("commits").join(dirname);
    fs::create_dir_all(&commits).unwrap();
    fs::write(commits.join("graph.bin"), b"stub").unwrap();
    let cm = ecp_core::registry::CommitBuildMeta {
        version: 1,
        sha: sha.to_string(),
        source_type: ecp_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 0,
        embedding_status: ecp_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
    };
    ecp_core::registry::CommitBuildMeta::write_atomic(&commits.join("meta.json"), &cm).unwrap();
}

fn setup_session(tmp: &Path, sid: &str, base_sha: &str, dirty: DirtyFiles) {
    let sd = tmp.join("sessions").join(sid);
    fs::create_dir_all(&sd).unwrap();
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
    DirtyFiles::write_atomic(&sd.join("dirty_files.json"), &dirty).unwrap();
}

fn one_dirty_entry() -> DirtyFiles {
    let mut df = DirtyFiles::empty();
    df.entries.insert(
        "src/a.rs".into(),
        DirtyEntry {
            mtime_ns: 1,
            content_hash: "deadbeef".into(),
            fragment_id: "frag1".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: vec![],
        },
    );
    df
}

const SHA: &str = "abc123def456789012345678901234567890abcd";
const DIRNAME: &str = "branch_main__abc123def456789012345678901234567890abcd";

#[test]
fn classify_empty_dirty_returns_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    match classify(tmp.path(), "sid1") {
        SessionState::PureReference {
            base_sha,
            l2_dirname,
        } => {
            assert_eq!(base_sha, SHA);
            assert_eq!(l2_dirname, DIRNAME);
        }
        other => panic!("expected PureReference, got {other:?}"),
    }
}

#[test]
fn classify_nonempty_dirty_returns_augmented() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, one_dirty_entry());
    match classify(tmp.path(), "sid1") {
        SessionState::AugmentedReference { fragment_count, .. } => {
            assert_eq!(fragment_count, 1);
        }
        other => panic!("expected AugmentedReference, got {other:?}"),
    }
}

#[test]
fn classify_missing_dirty_file_returns_pure_reference() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    let sd = tmp.path().join("sessions").join("sid1");
    fs::create_dir_all(&sd).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "sid1".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: SHA.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::PureReference { .. }
    ));
}

#[test]
fn classify_corrupt_dirty_returns_stale_dirtycorrupt() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    fs::write(
        tmp.path().join("sessions/sid1/dirty_files.json"),
        b"{ not valid json",
    )
    .unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale {
            reason: StaleReason::DirtyFilesCorrupt
        }
    ));
}

#[test]
fn classify_missing_meta_returns_stale_metaunreadable() {
    let tmp = tempfile::tempdir().unwrap();
    setup_repo(tmp.path(), SHA, DIRNAME);
    fs::create_dir_all(tmp.path().join("sessions/sid1")).unwrap();
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale {
            reason: StaleReason::MetaUnreadable
        }
    ));
}

#[test]
fn classify_missing_l2_returns_stale_l2missing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("commits")).unwrap();
    setup_session(tmp.path(), "sid1", SHA, DirtyFiles::empty());
    assert!(matches!(
        classify(tmp.path(), "sid1"),
        SessionState::Stale {
            reason: StaleReason::L2Missing
        }
    ));
}
