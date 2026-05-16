//! Integration tests for `gnx scan` — file-level hallucination check.
//!
//! Coverage:
//! - Clean file (all refs resolved) → "File OK, 0 unresolved references"
//! - File with typo ref → unresolved entry + did_you_mean suggestion
//! - Unknown language extension → error on stderr

mod common;

use common::run_git;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn build_index(root: &Path) -> std::path::PathBuf {
    let home = root.join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("gnx admin index failed to spawn");
    assert!(
        out.status.success(),
        "gnx admin index failed:\n  stderr={}\n  stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    home
}

/// Build a temp repo with a Python source file that has a known-good
/// function and a typo call. Returns (tempdir, home_path).
fn setup_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    std::fs::write(
        root.join("main.py"),
        concat!(
            "def validate_user(email):\n",
            "    return True\n",
            "\n",
            "def main():\n",
            "    validate_user('a@b.c')   # valid\n",
            "    valdiate_usr('oops')      # typo — should not resolve\n",
        ),
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    let home = build_index(root);

    (repo, home)
}

// ---------------------------------------------------------------------------

#[test]
fn scan_clean_file_reports_ok() {
    let (repo, home) = setup_repo();
    let root = repo.path();

    // Write a file that only calls the valid symbol.
    let clean = root.join("clean.py");
    std::fs::write(
        &clean,
        "from main import validate_user\nvalidate_user('x')\n",
    )
    .unwrap();

    let out = Command::new(gnx_bin())
        .args([
            "scan",
            clean.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 unresolved") || stdout.contains("File OK"),
        "expected OK status:\n  stdout={stdout}\n  stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn scan_invalid_ref_lists_with_suggestion() {
    let (repo, home) = setup_repo();
    let root = repo.path();

    let out = Command::new(gnx_bin())
        .args([
            "scan",
            root.join("main.py").to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("valdiate_usr"),
        "expected typo to appear as unresolved:\n  stdout={stdout}\n  stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    // Accept either the JSON key or the corrected name appearing in output.
    assert!(
        stdout.contains("did_you_mean") || stdout.contains("validate_user"),
        "expected fuzzy suggestion in output:\n  stdout={stdout}",
    );
}

#[test]
fn scan_unknown_language_errors() {
    let (repo, home) = setup_repo();
    let root = repo.path();

    let weird = root.join("file.unknown");
    std::fs::write(&weird, "some content").unwrap();

    let out = Command::new(gnx_bin())
        .args([
            "scan",
            weird.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown language")
            || stderr.contains("unsupported")
            || stderr.contains("scan failed"),
        "expected error about unknown language:\n  stderr={stderr}",
    );
}
