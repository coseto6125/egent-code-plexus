//! Phase 3 — persistent per-file parse cache tests.
//!
//! Caches tree-sitter `LocalGraph` blobs keyed by (path, content_hash). On hit,
//! the analyzer pipeline skips the parse step entirely and feeds the
//! cached graph straight into the global builder. Binary upgrade → new
//! `BUILDER_FINGERPRINT` → new cache subdir → old entries stay on disk
//! but become unreachable (until a future GC sweeps them).

use ecp_cli::parse_cache::ParseCache;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

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
        path_literals: None,
        sql_refs: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

#[test]
fn empty_cache_returns_none_on_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();
    assert!(cache.get(Path::new("src/a.rs"), &[0u8; 8]).is_none());
}

#[test]
fn put_then_get_round_trips_local_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let mut hash = [0u8; 8];
    hash[0] = 1;
    let g = graph("src/a.rs", hash);
    cache.put(&g).unwrap();

    let got = cache
        .get(Path::new("src/a.rs"), &hash)
        .expect("cached entry should hit");
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

    let a = cache.get(Path::new("a.rs"), &h1).unwrap();
    let b = cache.get(Path::new("b.rs"), &h2).unwrap();
    assert_eq!(a.file_path.to_str(), Some("a.rs"));
    assert_eq!(b.file_path.to_str(), Some("b.rs"));
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
    let path = cache.path_for(Path::new("x.rs"), &hash);
    std::fs::write(&path, b"not-a-valid-rkyv-blob").unwrap();
    assert!(path.exists());

    assert!(cache.get(Path::new("x.rs"), &hash).is_none());
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

    let blob = cache.path_for(Path::new("c.rs"), &hash);
    let parse_cache_dir = blob.parent().unwrap().parent().unwrap();
    assert_eq!(parse_cache_dir.file_name().unwrap(), "parse_cache");

    // Drop a blob into a sibling fingerprint dir — must not be visible.
    let stale_fp_dir = parse_cache_dir.join("deadbeef");
    std::fs::create_dir_all(&stale_fp_dir).unwrap();
    std::fs::write(stale_fp_dir.join(blob.file_name().unwrap()), b"x").unwrap();

    assert_eq!(
        cache
            .get(Path::new("c.rs"), &hash)
            .unwrap()
            .file_path
            .to_str(),
        Some("c.rs"),
        "current-fingerprint entry must win over stale sibling fingerprint dir"
    );
}

/// Two files can hold the same bytes — an empty `__init__.py`, a generated
/// stub, a duplicated config. Before the path joined the key, the second file
/// read back the first one's `LocalGraph`, so its symbols arrived carrying the
/// wrong `file_path`, collided on uid with the originals, and were tombstoned
/// to empty names. The file then had no presence in the graph at all while
/// `ecp find` still reported success.
///
/// The assertion is on what the caller receives — the path it asked about —
/// rather than on the key's spelling, so any future keying scheme that keeps
/// the two files apart passes.
#[test]
fn byte_identical_files_at_different_paths_keep_their_own_graphs() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let same_bytes = [9u8; 8];
    cache.put(&graph("src/a.py", same_bytes)).unwrap();
    cache.put(&graph("src/dup.py", same_bytes)).unwrap();

    assert_eq!(
        cache
            .get(Path::new("src/dup.py"), &same_bytes)
            .expect("the duplicate's own entry must exist")
            .file_path
            .to_str(),
        Some("src/dup.py"),
        "the duplicate read back the original's graph"
    );
    assert_eq!(
        cache
            .get(Path::new("src/a.py"), &same_bytes)
            .expect("the original's entry must survive the duplicate's put")
            .file_path
            .to_str(),
        Some("src/a.py"),
        "the duplicate's put overwrote the original's entry"
    );
}

/// A file that moves keeps its bytes, so a content-only key would hand the new
/// path the old path's graph. It must miss instead and be reparsed.
#[test]
fn a_renamed_file_does_not_read_back_its_old_path() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let unchanged = [4u8; 8];
    cache.put(&graph("src/before.rs", unchanged)).unwrap();

    assert!(
        cache.get(Path::new("src/after.rs"), &unchanged).is_none(),
        "the new path must miss, not inherit the old path's graph"
    );
}

/// On Unix a backslash is an ordinary filename byte, so `src\dup.rs` and
/// `src/dup.rs` are two different files. An earlier version of the key folded
/// `\` to `/` for cross-platform tidiness and reintroduced, for that pair, the
/// exact collision the path was added to prevent.
#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_path_is_not_a_separator() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ParseCache::open(tmp.path()).unwrap();

    let same_bytes = [5u8; 8];
    cache.put(&graph(r"src\dup.rs", same_bytes)).unwrap();
    cache.put(&graph("src/dup.rs", same_bytes)).unwrap();

    assert_eq!(
        cache
            .get(Path::new(r"src\dup.rs"), &same_bytes)
            .expect("the backslash path keeps its own entry")
            .file_path
            .to_str(),
        Some(r"src\dup.rs")
    );
    assert_eq!(
        cache
            .get(Path::new("src/dup.rs"), &same_bytes)
            .expect("the slash path keeps its own entry")
            .file_path
            .to_str(),
        Some("src/dup.rs")
    );
}
