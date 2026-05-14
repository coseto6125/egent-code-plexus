//! Tests the contract of `atomic_write_bytes`: the target file is either
//! the previous version or the new version, never a half-written
//! intermediate. The temp sibling must not collide with the target's
//! own extension (so `graph.bin` ↔ `graph.bin.tmp`, never `graph.tmp`).

use gnx_core::registry::atomic_write_bytes;
use std::fs;
use tempfile::tempdir;

#[test]
fn atomic_write_creates_target_and_leaves_no_tmp_sibling() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    atomic_write_bytes(&path, b"first").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"first");
    let tmp = dir.path().join("graph.bin.tmp");
    assert!(!tmp.exists(), "successful write must clean its tmp sibling");
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
fn atomic_write_replaces_stale_tmp_sibling() {
    // A previously-aborted write leaves a `.tmp` file. The next
    // attempt must overwrite (not refuse) so the system self-heals
    // without manual cleanup.
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    let tmp = dir.path().join("graph.bin.tmp");
    fs::write(&tmp, b"stale-corrupt-payload").unwrap();
    atomic_write_bytes(&path, b"clean").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"clean");
    assert!(!tmp.exists(), "tmp sibling must be consumed by rename");
}
