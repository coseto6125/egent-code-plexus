use cgn_core::registry::RepoMeta;
use std::collections::BTreeMap;
use tempfile::NamedTempFile;

#[test]
fn round_trip_btreemap_deterministic() {
    let mut refs = BTreeMap::new();
    refs.insert("refs/heads/main".to_string(), "abc123".to_string());
    refs.insert("refs/tags/v1".to_string(), "def456".to_string());
    let m = RepoMeta {
        version: 1,
        common_dir: "/work/r/.git".into(),
        remote_url: Some("https://github.com/u/r.git".into()),
        aliases: vec!["r".into()],
        known_refs: refs.clone(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    let s1 = serde_json::to_string(&m).unwrap();
    let s2 = serde_json::to_string(&m).unwrap();
    assert_eq!(s1, s2);
    // BTreeMap → JSON keys must be sorted
    let i = s1.find("refs/heads/main").unwrap();
    let j = s1.find("refs/tags/v1").unwrap();
    assert!(i < j, "BTreeMap iterates in sorted key order");

    let back: RepoMeta = serde_json::from_str(&s1).unwrap();
    assert_eq!(back, m);
}

#[test]
fn atomic_write_round_trip_full_struct_equality() {
    let tmp = NamedTempFile::new().unwrap();
    let m = RepoMeta {
        version: 1,
        common_dir: "/x".into(),
        remote_url: None,
        aliases: vec![],
        known_refs: BTreeMap::new(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    RepoMeta::write_atomic(tmp.path(), &m).unwrap();
    let r = RepoMeta::read(tmp.path()).unwrap();
    assert_eq!(r, m);
}

#[test]
fn missing_aliases_and_known_refs_default_to_empty() {
    let tmp = NamedTempFile::new().unwrap();
    // Hand-crafted JSON without aliases / known_refs fields — should still deserialize
    let json = r#"{
        "version": 1,
        "common_dir": "/x",
        "remote_url": null,
        "last_built_sha": null,
        "total_size_bytes": 0,
        "last_touched": "2026-05-17T10:00:00Z"
    }"#;
    std::fs::write(tmp.path(), json).unwrap();
    let r = RepoMeta::read(tmp.path()).unwrap();
    assert!(r.aliases.is_empty());
    assert!(r.known_refs.is_empty());
}
