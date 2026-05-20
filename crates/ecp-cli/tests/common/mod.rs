//! Shared scaffolding for integration tests. Lives under `tests/common/`
//! so `cargo test` doesn't treat it as a standalone test binary.

#![allow(dead_code)]

pub mod peer_harness;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a `git` invocation and panic with stderr if it fails. Used by every
/// integration test that builds a tempdir repo + indexes it via `ecp admin
/// index`. The wrapping `setup_repo*` functions stay per-file because their
/// fixtures (file layout, branch name, remote, multi-file vs single) vary
/// per test in ways an all-purpose helper would obscure.
pub fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

pub fn write_graph(dir: &Path, bytes: &[u8]) -> PathBuf {
    let path = dir.join("graph.bin");
    std::fs::write(&path, bytes).unwrap();
    path
}
