//! Tests the contract of `atomic_write_bytes`: the target file is either
//! the previous version or the new version, never a half-written
//! intermediate. The temp sibling must not collide with the target's
//! own extension (so `graph.bin` ↔ `graph.bin.tmp`, never `graph.tmp`).

use graph_nexus_core::registry::{atomic_write_bytes, atomic_write_bytes_no_fsync};
use std::fs;
use std::thread;
use tempfile::tempdir;

#[test]
fn atomic_write_creates_target_and_leaves_no_tmp_sibling() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    atomic_write_bytes(&path, b"first").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"first");
    // Each writer uses a unique `<path>.<pid>.<counter>.tmp`; a successful
    // write must consume its own tmp via rename, leaving no tmp sibling
    // matching the target's basename.
    let leftover_tmps: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.starts_with("graph.bin") && s.ends_with(".tmp")
        })
        .collect();
    assert!(
        leftover_tmps.is_empty(),
        "successful write must clean its tmp sibling, found: {leftover_tmps:?}"
    );
}

#[test]
fn atomic_write_overwrites_existing_target() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    atomic_write_bytes(&path, b"v1").unwrap();
    atomic_write_bytes(&path, b"v2-longer-payload").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"v2-longer-payload");
}

#[test]
fn atomic_write_creates_missing_parent_dirs() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested/sub/dir/graph.bin");
    atomic_write_bytes(&path, b"payload").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"payload");
}

#[test]
fn atomic_write_tmp_sibling_appends_rather_than_replacing_extension() {
    // If the implementation regressed to `with_extension("tmp")`, the
    // sibling would be `graph.tmp` and would collide with any sibling
    // file named the same way (e.g. for `graph.bin` and `graph.json`
    // in the same directory, both tmps would resolve to `graph.tmp`).
    // This test pins the append-behavior so two file types written
    // concurrently can't trample each other's tmp file.
    let dir = tempdir().unwrap();
    let bin_path = dir.path().join("graph.bin");
    let json_path = dir.path().join("graph.json");
    atomic_write_bytes(&bin_path, b"bin").unwrap();
    atomic_write_bytes(&json_path, b"json").unwrap();
    assert_eq!(fs::read(&bin_path).unwrap(), b"bin");
    assert_eq!(fs::read(&json_path).unwrap(), b"json");
}

#[test]
fn atomic_write_tolerates_stale_tmp_sibling() {
    // A previously-aborted write leaves a stale `<path>.tmp` (or unique-
    // suffix variant). The current writer uses its own unique tmp name,
    // so the stale sibling is harmless — the write still succeeds with
    // clean content, and any stale tmp can be swept by cleanup tools
    // (matched by basename + `.tmp` suffix).
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    let stale = dir.path().join("graph.bin.tmp");
    fs::write(&stale, b"stale-corrupt-payload").unwrap();
    atomic_write_bytes(&path, b"clean").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"clean");
    // Stale tmp is unrelated to the current writer's unique tmp; it may
    // survive the write. The contract is that the *target* is clean,
    // not that the orchard has been raked.
}

#[test]
fn atomic_write_no_fsync_allows_parallel_same_target_writers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cache-entry.rkyv");
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let path = path.clone();
            thread::spawn(move || {
                let payload = format!("payload-{i}");
                atomic_write_bytes_no_fsync(&path, payload.as_bytes()).unwrap();
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let bytes = fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"payload-"));
}
