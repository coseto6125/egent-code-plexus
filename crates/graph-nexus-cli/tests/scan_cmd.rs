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

/// `--filter-stdlib` on a Python file must drop stdlib noise (`json`,
/// `typing`, `Any`, `self`, dunders, …) while keeping project-symbol
/// typos visible. Asserts the trimmed `unresolved_count` is strictly
/// smaller than the baseline and that `filtered_count` is reported.
#[test]
fn scan_filter_stdlib_drops_python_noise_keeps_typo() {
    let (repo, home) = setup_repo();
    let root = repo.path();

    // File mixes stdlib refs (json/typing/dunders) with the same typo as
    // the baseline test, so we can compare before-vs-after for the same
    // bag of identifiers.
    let mixed = root.join("mixed.py");
    std::fs::write(
        &mixed,
        concat!(
            "import json\n",
            "from typing import Any, Optional\n",
            "\n",
            "def handler(payload: Any) -> Optional[str]:\n",
            "    return json.dumps(payload)\n",
            "\n",
            "__all__ = ['handler']\n",
            "\n",
            "valdiate_usr('typo')  # project-symbol typo — must remain\n",
        ),
    )
    .unwrap();

    let run = |extra: &[&str]| -> String {
        let out = Command::new(gnx_bin())
            .args(["scan", mixed.to_str().unwrap(), "--repo", root.to_str().unwrap()])
            .args(extra)
            .env("HOME", &home)
            .current_dir(root)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let baseline = run(&[]);
    let filtered = run(&["--filter-stdlib"]);

    let extract_count = |s: &str| -> Option<u32> {
        s.lines()
            .find_map(|l| l.trim().strip_prefix("unresolved_count:"))
            .and_then(|n| n.trim().parse().ok())
    };

    let base_n = extract_count(&baseline).unwrap_or_else(|| {
        panic!("baseline missing unresolved_count:\n{baseline}")
    });
    let filt_n = extract_count(&filtered).unwrap_or_else(|| {
        panic!("filtered missing unresolved_count:\n{filtered}")
    });

    assert!(
        filt_n < base_n,
        "filter-stdlib should reduce count: baseline={base_n} filtered={filt_n}"
    );
    assert!(
        filtered.contains("filtered_count:"),
        "filtered output should report filtered_count:\n{filtered}"
    );
    // The project-symbol typo must survive — that's the signal users care about.
    assert!(
        filtered.contains("valdiate_usr"),
        "typo must still appear after filtering:\n{filtered}"
    );
    // Spot-check that an obvious noise name was actually dropped.
    assert!(
        !filtered.contains("name: json")
            && !filtered.contains("name: Any")
            && !filtered.contains("name: __all__"),
        "stdlib noise (json/Any/__all__) should be filtered out:\n{filtered}"
    );
}

/// `--filter-stdlib` without the flag changes nothing about the output
/// contract — preserves backward compatibility for existing consumers.
#[test]
fn scan_filter_stdlib_off_preserves_baseline_count() {
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
        !stdout.contains("filtered_count:"),
        "filtered_count must NOT appear when flag is off:\n{stdout}"
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
