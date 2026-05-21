//! Phase 3 — persistent per-file parse cache tests.
//!
//! Caches tree-sitter `LocalGraph` blobs keyed by content_hash. On hit,
//! the analyzer pipeline skips the parse step entirely and feeds the
//! cached graph straight into the global builder. Binary upgrade → new
//! `BUILDER_FINGERPRINT` → new cache subdir → old entries stay on disk
//! but become unreachable (until a future GC sweeps them).

use ecp_cli::parse_cache::ParseCache;
use ecp_core::analyzer::types::LocalGraph;

fn graph(file: &str, hash: [u8; 8]) -> LocalGraph {
    LocalGraph {
        file_path: file.into(),
        content_hash: hash,
        nodes: vec![],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        call_metas: vec![],
    }
}

#[test]
fn empty_cache_returns_none_on_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();
    assert!(cache.get(&[0u8; 8]).is_none());
}

#[test]
fn put_then_get_round_trips_local_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let mut hash = [0u8; 8];
    hash[0] = 1;
    let g = graph("src/a.rs", hash);
    cache.put(&g).unwrap();

    let got = cache.get(&hash).expect("cached entry should hit");
    assert_eq!(got.content_hash, hash);
    assert_eq!(got.file_path.to_str(), Some("src/a.rs"));
}

#[test]
fn distinct_hashes_dont_collide() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let mut h1 = [0u8; 8];
    h1[0] = 1;
    let mut h2 = [0u8; 8];
    h2[0] = 2;

    cache.put(&graph("a.rs", h1)).unwrap();
    cache.put(&graph("b.rs", h2)).unwrap();

    assert_eq!(cache.get(&h1).unwrap().file_path.to_str(), Some("a.rs"));
    assert_eq!(cache.get(&h2).unwrap().file_path.to_str(), Some("b.rs"));
}

#[test]
fn corrupted_entry_yields_miss_and_is_purged() {
    // Garbage bytes at the expected key path → rkyv deserialize fails →
    // get returns None AND the bad file is removed so the next put can
    // refresh it cleanly (a stale corrupt file otherwise re-poisons the
    // slot indefinitely).
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let mut hash = [0u8; 8];
    hash[0] = 7;
    let path = cache.path_for(&hash);
    std::fs::write(&path, b"not-a-valid-rkyv-blob").unwrap();
    assert!(path.exists());

    assert!(cache.get(&hash).is_none());
    assert!(!path.exists(), "corrupted blob must be removed on miss");
}

#[test]
fn fingerprint_scopes_cache_entries_by_subdirectory() {
    // The cache root inserts a fingerprint-derived subdir between
    // `parse_cache/` and the blob. Verifies a binary upgrade (manually
    // emulated by writing into a different fingerprint dir) does not
    // expose stale entries to the running binary.
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let mut hash = [0u8; 8];
    hash[0] = 3;
    cache.put(&graph("c.rs", hash)).unwrap();

    let blob = cache.path_for(&hash);
    let parse_cache_dir = blob.parent().unwrap().parent().unwrap();
    assert_eq!(parse_cache_dir.file_name().unwrap(), "parse_cache");

    // Drop a blob into a sibling fingerprint dir — must not be visible.
    let stale_fp_dir = parse_cache_dir.join("deadbeef");
    std::fs::create_dir_all(&stale_fp_dir).unwrap();
    std::fs::write(
        stale_fp_dir.join(format!("{:016x}.rkyv", u64::from_le_bytes(hash))),
        b"x",
    )
    .unwrap();

    assert_eq!(
        cache.get(&hash).unwrap().file_path.to_str(),
        Some("c.rs"),
        "current-fingerprint entry must win over stale sibling fingerprint dir"
    );
}
