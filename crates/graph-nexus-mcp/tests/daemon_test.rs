use graph_nexus_mcp::daemon::needs_remap;
use std::fs;
use std::time::SystemTime;
use tempfile::TempDir;

#[test]
fn needs_remap_false_when_mtime_unchanged() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.bin");
    fs::write(&path, b"v1").unwrap();
    let loaded_at = fs::metadata(&path).unwrap().modified().unwrap();
    assert!(needs_remap(&path, loaded_at).unwrap().is_none());
}

#[test]
fn needs_remap_true_when_file_atomically_replaced() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.bin");
    fs::write(&path, b"v1").unwrap();
    let loaded_at = fs::metadata(&path).unwrap().modified().unwrap();

    // Atomic replace via rename — mtime of the path's new inode is later.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let tmp = dir.path().join("graph.bin.tmp");
    fs::write(&tmp, b"v2").unwrap();
    fs::rename(&tmp, &path).unwrap();

    assert!(needs_remap(&path, loaded_at).unwrap().is_some());
}

#[test]
fn needs_remap_errors_if_path_missing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("never_existed.bin");
    let res = needs_remap(&path, SystemTime::UNIX_EPOCH);
    assert!(res.is_err());
}
