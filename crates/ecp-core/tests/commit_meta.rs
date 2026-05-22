use ecp_core::registry::{CommitBuildMeta, EmbeddingStatus, RefRecord, SourceType};

#[test]
fn round_trip_deterministic_json() {
    let meta = CommitBuildMeta {
        version: 1,
        sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_type: SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/work/myrepo".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: Some("def0000000000000000000000000000000000000".into()),
        node_count: 100,
        embedding_status: EmbeddingStatus::None,
        refs_at_build: vec![RefRecord {
            ref_name: "refs/heads/main".into(),
            seen_at: "2026-05-17T10:00:00Z".into(),
        }],
        refs_seen_since: vec![],
        builder_fingerprint: None,
        binary_commit_sha: None,
    };
    let s1 = serde_json::to_string(&meta).unwrap();
    let s2 = serde_json::to_string(&meta).unwrap();
    assert_eq!(s1, s2, "serialization must be deterministic");
    let back: CommitBuildMeta = serde_json::from_str(&s1).unwrap();
    assert_eq!(back, meta);
}

#[test]
fn atomic_write_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("commit_meta.json");
    let meta = CommitBuildMeta {
        version: 1,
        sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_type: SourceType::Commit,
        source_id: None,
        built_from_worktree: "/work/x".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 42,
        embedding_status: EmbeddingStatus::Skipped,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
        binary_commit_sha: None,
    };
    CommitBuildMeta::write_atomic(&path, &meta).unwrap();
    let read = CommitBuildMeta::read(&path).unwrap();
    assert_eq!(read, meta);
}

#[test]
fn embedding_status_computed_round_trips() {
    let meta = CommitBuildMeta {
        version: 1,
        sha: "abc123def4567890abc123def4567890abc123de".into(),
        source_type: SourceType::Tag,
        source_id: Some("v1.0".into()),
        built_from_worktree: "/work/y".into(),
        built_at: "2026-05-17T11:00:00Z".into(),
        parent_sha: None,
        node_count: 7,
        embedding_status: EmbeddingStatus::Computed,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
        binary_commit_sha: None,
    };
    let s = serde_json::to_string(&meta).unwrap();
    let back: CommitBuildMeta = serde_json::from_str(&s).unwrap();
    assert!(matches!(back.embedding_status, EmbeddingStatus::Computed));
}
