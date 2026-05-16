//! Shared scaffolding for integration tests. Lives under `tests/common/`
//! so `cargo test` doesn't treat it as a standalone test binary.

use std::path::Path;
use std::process::Command;

/// Run a `git` invocation and panic with stderr if it fails. Used by every
/// integration test that builds a tempdir repo + indexes it via `gnx admin
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
