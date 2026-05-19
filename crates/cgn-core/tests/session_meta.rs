use cgn_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::collections::BTreeMap;
use tempfile::NamedTempFile;

#[test]
fn session_meta_round_trip() {
    let sm = SessionMeta {
        version: 1,
        session_id: "cli-abc12345".into(),
        pid: Some(1234),
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:30:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/myrepo".into(),
        overlay_version: 5,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    let s1 = serde_json::to_string(&sm).unwrap();
    let s2 = serde_json::to_string(&sm).unwrap();
    assert_eq!(s1, s2);
    let back: SessionMeta = serde_json::from_str(&s1).unwrap();
    assert_eq!(back, sm);
}

#[test]
fn dirty_files_deterministic_btreemap_order() {
    let mut entries = BTreeMap::new();
    entries.insert(
        "src/a.rs".into(),
        DirtyEntry {
            mtime_ns: 1000,
            content_hash: "deadbeef".into(),
            fragment_id: "frag1".into(),
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: vec![],
        },
    );
    entries.insert(
        "src/b.rs".into(),
        DirtyEntry {
            mtime_ns: 2000,
            content_hash: "cafebabe".into(),
            fragment_id: "frag2".into(),
            tantivy_delta_segment: Some("seg_xxx".into()),
            parse_failed: false,
            dirty_symbols: vec![],
        },
    );
    let df = DirtyFiles {
        version: 1,
        entries,
    };
    let s1 = serde_json::to_string(&df).unwrap();
    let s2 = serde_json::to_string(&df).unwrap();
    assert_eq!(s1, s2);
    assert!(
        s1.find("src/a.rs").unwrap() < s1.find("src/b.rs").unwrap(),
        "BTreeMap → keys sorted"
    );
    let back: DirtyFiles = serde_json::from_str(&s1).unwrap();
    assert_eq!(back, df);
}

#[test]
fn atomic_write_session_meta_full_equality() {
    let tmp = NamedTempFile::new().unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "x".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "0".repeat(40),
        source_worktree: "/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(tmp.path(), &sm).unwrap();
    let r = SessionMeta::read(tmp.path()).unwrap();
    assert_eq!(r, sm);
}

#[test]
fn dirty_files_empty_helper() {
    let df = DirtyFiles::empty();
    assert_eq!(df.version, 1);
    assert!(df.entries.is_empty());
}

#[test]
fn missing_parse_failed_defaults_to_false() {
    let json = r#"{
        "version": 1,
        "entries": {
            "src/a.rs": {
                "mtime_ns": 1000,
                "content_hash": "deadbeef",
                "fragment_id": "frag1",
                "tantivy_delta_segment": null
            }
        }
    }"#;
    let df: DirtyFiles = serde_json::from_str(json).unwrap();
    let entry = df.entries.get("src/a.rs").unwrap();
    assert!(!entry.parse_failed);
}
