//! Shared fixtures for MCP integration tests.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Write an executable shell script (stub `cgn`) into `dir` and return its path.
pub fn write_stub(dir: &Path, script: &str) -> PathBuf {
    let stub = dir.join("cgn");
    std::fs::write(&stub, script).unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    stub
}
