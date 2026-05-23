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

/// Write `body` to `repo/rel`, creating parent directories. Panics on
/// any filesystem error so tests fail fast.
pub fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

/// Init `repo` as a single-commit git repo on `main` and run `ecp
/// admin index --repo .` with `HOME=<repo>` so the registry lives
/// inside the tempdir. This is the canonical setup for fixtures that
/// need an indexed graph; per-file variants are only justified when
/// the test needs a different branch, multiple commits, a remote, or
/// other non-default git state.
pub fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
