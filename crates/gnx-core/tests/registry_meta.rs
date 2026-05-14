//! Tests for per-branch meta.json (spec §1, §2.1 rebuild-from-disk).

use gnx_core::registry::BranchMeta;

#[test]
fn meta_round_trip() {
    let original = BranchMeta {
        indexed_at: "2026-05-14T03:00:00Z".into(),
        node_count: 12453,
        delta_size: 0,
        last_compact_at: Some("2026-05-14T02:00:00Z".into()),
        worktree_path: "/home/enor/gitnexus-rs".into(),
        remote_url: "git@github.com:E-NoR/gitnexus-rs.git".into(),
        schema_version: 1,
    };
    let json = serde_json::to_string(&original).unwrap();
    let parsed: BranchMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn meta_atomic_write_then_read() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("meta.json");
    let meta = BranchMeta {
        indexed_at: "2026-05-14T03:00:00Z".into(),
        node_count: 100,
        delta_size: 0,
        last_compact_at: None,
        worktree_path: "/x".into(),
        remote_url: "".into(),
        schema_version: 1,
    };
    BranchMeta::write_atomic(&path, &meta).unwrap();
    let r = BranchMeta::read(&path).unwrap();
    assert_eq!(r, meta);
}
