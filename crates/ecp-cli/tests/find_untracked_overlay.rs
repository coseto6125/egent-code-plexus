//! FU-2026-06-10-8b98d5e991a6: symbols in UNTRACKED files must be visible to
//! queries the same way tracked-but-uncommitted edits are (L1 overlay merge).
//!
//! Before the fix, `git_fingerprint_shortcut` ran `git status --porcelain
//! --untracked-files=no`, so a repo whose only change was a brand-new file
//! reported Ready — no overlay reanalysis ever saw the file, and `ecp find`
//! answered a definitive-looking `found:false` for code sitting right there
//! on disk. The agent workflow this breaks is the most common one: Write a
//! new file, then query it.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::path::Path;
use std::process::Command;

fn init_and_index(repo: &Path, home: &Path) {
    fs::write(repo.join("lib.rs"), "pub fn base_fn() {}\n").unwrap();
    fs::write(repo.join(".gitignore"), "scratch/\n").unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    commit_all(repo, "init");
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find_exact(repo: &Path, home: &Path, pattern: &str) -> serde_json::Value {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(["find", pattern, "--format", "json"])
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    // Pin the bare-CLI session path: with no host session id, the overlay
    // writer and reader must still agree on one per-process fallback id.
    // (A per-CALL fallback id sent fragments to one session dir and read
    // another — overlay invisible exactly when no agent host is present.)
    for key in [
        "ECP_SESSION_ID",
        "GEMINI_CLI_SESSION_ID",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("find failed to spawn");
    assert!(
        out.status.success(),
        "find {pattern} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "non-JSON find output ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn find_sees_symbol_in_untracked_file() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::write(
        repo_tmp.path().join("newfile.rs"),
        "pub fn untracked_marker_fn() {}\n",
    )
    .unwrap();

    let v = find_exact(repo_tmp.path(), home_tmp.path(), "untracked_marker_fn");
    assert_eq!(
        v["found"], true,
        "a symbol in an untracked file must be found via the L1 overlay: {v}"
    );
}

/// Pins `--untracked-files=all` over `normal`: with `normal`, git lists a new
/// directory as one collapsed `?? dir/` entry, so the file inside never makes
/// it into the dirty set and the overlay misses it.
#[test]
fn find_sees_symbol_inside_untracked_dir() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::create_dir(repo_tmp.path().join("newmod")).unwrap();
    fs::write(
        repo_tmp.path().join("newmod/inner.rs"),
        "pub fn nested_untracked_fn() {}\n",
    )
    .unwrap();

    let v = find_exact(repo_tmp.path(), home_tmp.path(), "nested_untracked_fn");
    assert_eq!(
        v["found"], true,
        "a symbol inside an untracked directory must be found via the L1 overlay: {v}"
    );
}

/// Gitignored scratch files must stay OUT: porcelain honours .gitignore, so
/// ignored files never enter the dirty set, never burn reanalysis time, and
/// never pollute query results.
#[test]
fn gitignored_untracked_file_stays_invisible() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_and_index(repo_tmp.path(), home_tmp.path());

    fs::create_dir(repo_tmp.path().join("scratch")).unwrap();
    fs::write(
        repo_tmp.path().join("scratch/junk.rs"),
        "pub fn ignored_scratch_fn() {}\n",
    )
    .unwrap();

    let v = find_exact(repo_tmp.path(), home_tmp.path(), "ignored_scratch_fn");
    assert_eq!(
        v["found"], false,
        "gitignored files must not leak into query results: {v}"
    );
}
