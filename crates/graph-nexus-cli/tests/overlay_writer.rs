use graph_nexus_cli::session::overlay_writer::{write_dirty_fragment, FragmentInput};
use graph_nexus_core::session::{DirtyFiles, SessionMeta};

fn make_session_dir(tmp: &std::path::Path, sid: &str) -> std::path::PathBuf {
    let session_dir = tmp.join("sessions").join(sid);
    std::fs::create_dir_all(&session_dir).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.to_string(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_worktree: "/work/x".into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&session_dir.join("session_meta.json"), &sm).unwrap();
    let df = DirtyFiles::empty();
    DirtyFiles::write_atomic(&session_dir.join("dirty_files.json"), &df).unwrap();
    session_dir
}

#[test]
fn first_dirty_file_creates_fragment_and_manifest_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "test-sid");

    let input = FragmentInput {
        rel_path: "src/a.rs".into(),
        content: b"fn a() {}".to_vec(),
        mtime_ns: 1000,
    };
    let outcome = write_dirty_fragment(&session_dir, &input).unwrap();
    assert!(!outcome.parse_failed);

    // Fragment file exists
    let frags: Vec<_> = std::fs::read_dir(session_dir.join("graph_overlay"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(frags.len(), 1);

    // Manifest updated with this file
    let df = DirtyFiles::read(&session_dir.join("dirty_files.json")).unwrap();
    assert!(df.entries.contains_key("src/a.rs"));
    let entry = &df.entries["src/a.rs"];
    assert_eq!(entry.mtime_ns, 1000);
    assert!(!entry.parse_failed);
    assert_eq!(entry.fragment_id, outcome.fragment_id);
}

#[test]
fn overlay_version_bumps_on_each_write() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "v-sid");

    let input1 = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"x".to_vec(),
        mtime_ns: 1,
    };
    write_dirty_fragment(&session_dir, &input1).unwrap();
    let sm1 = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm1.overlay_version, 1);

    let input2 = FragmentInput {
        rel_path: "b.rs".into(),
        content: b"y".to_vec(),
        mtime_ns: 2,
    };
    write_dirty_fragment(&session_dir, &input2).unwrap();
    let sm2 = SessionMeta::read(&session_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm2.overlay_version, 2);
}

#[test]
fn content_hash_drives_fragment_id() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "h-sid");

    let input_a = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"hello".to_vec(),
        mtime_ns: 1,
    };
    let out_a = write_dirty_fragment(&session_dir, &input_a).unwrap();

    let input_b = FragmentInput {
        rel_path: "b.rs".into(),
        content: b"hello".to_vec(), // same content
        mtime_ns: 2,
    };
    let out_b = write_dirty_fragment(&session_dir, &input_b).unwrap();

    // Same content → same fragment_id (the file maps to its content's hash)
    assert_eq!(out_a.fragment_id, out_b.fragment_id);
}

#[test]
fn same_file_re_written_same_content_idempotent_id() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = make_session_dir(tmp.path(), "i-sid");

    let input = FragmentInput {
        rel_path: "a.rs".into(),
        content: b"same".to_vec(),
        mtime_ns: 1,
    };
    let out1 = write_dirty_fragment(&session_dir, &input).unwrap();
    let out2 = write_dirty_fragment(&session_dir, &input).unwrap();
    assert_eq!(out1.fragment_id, out2.fragment_id);
}
