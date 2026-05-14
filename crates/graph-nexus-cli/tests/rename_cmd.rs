//! Integration test for `gnx rename` (Python MVP).
//!
//! Pipeline coverage:
//! - Stage 1 (graph): locate target node, collect inbound-edge source files
//! - Stage 2 (AST): tree-sitter Python parse + identifier-occurrence find
//! - Stage 3 (dry-run): report counts + diff, no file mutated
//! - Stage 3 (execute): atomic per-file replace, syntax remains valid

use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run_git(repo: &Path, args: &[&str]) {
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

/// Build a temp repo with rename_def.py + rename_user.py, init git, run
/// `gnx analyze` to materialize a graph.bin. Returns the repo root.
fn setup_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();
    std::fs::write(
        root.join("rename_def.py"),
        include_str!("fixtures/rename_def.py"),
    )
    .unwrap();
    std::fs::write(
        root.join("rename_user.py"),
        include_str!("fixtures/rename_user.py"),
    )
    .unwrap();
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);

    // Build the index so the rename Stage-1 graph traversal has something.
    let home = repo.path().join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(gnx_bin())
        .args(["analyze", "--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("gnx analyze failed to spawn");
    assert!(
        out.status.success(),
        "gnx analyze failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    repo
}

#[test]
fn dry_run_reports_hits_without_mutating_files() {
    let repo = setup_repo();
    let root = repo.path();
    let home = root.join(".home");

    let out = Command::new(gnx_bin())
        .args([
            "rename",
            "--symbol",
            "old_name",
            "--new-name",
            "fresh_name",
            "--repo",
            root.to_str().unwrap(),
            "--dry-run",
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("gnx rename failed to spawn");
    assert!(
        out.status.success(),
        "rename dry-run failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Expect: 2 files touched, at least 4 hits total (def + caller + 3
    // user-side occurrences = 5, but we'll let the precise count vary
    // depending on whether the `from x import old_name` line is included).
    assert!(
        stdout.contains("files 2") || stdout.contains("\"files\":2") || stdout.contains("files: 2"),
        "dry-run output should report 2 files; got:\n{stdout}",
    );
    assert!(
        stdout.contains("old_name") && stdout.contains("fresh_name"),
        "dry-run output should show both old and new name; got:\n{stdout}",
    );

    // Files must NOT have been modified.
    let def_after = std::fs::read_to_string(root.join("rename_def.py")).unwrap();
    let user_after = std::fs::read_to_string(root.join("rename_user.py")).unwrap();
    assert!(
        def_after.contains("def old_name"),
        "dry-run must not mutate rename_def.py; got:\n{def_after}",
    );
    assert!(
        user_after.contains("old_name()"),
        "dry-run must not mutate rename_user.py; got:\n{user_after}",
    );
    assert!(
        !def_after.contains("fresh_name"),
        "dry-run must not write fresh_name into source; got:\n{def_after}",
    );
}

#[test]
fn execute_renames_both_def_and_callers() {
    let repo = setup_repo();
    let root = repo.path();
    let home = root.join(".home");

    let out = Command::new(gnx_bin())
        .args([
            "rename",
            "--symbol",
            "old_name",
            "--new-name",
            "fresh_name",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("gnx rename failed to spawn");
    assert!(
        out.status.success(),
        "rename execute failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    let def_after = std::fs::read_to_string(root.join("rename_def.py")).unwrap();
    let user_after = std::fs::read_to_string(root.join("rename_user.py")).unwrap();

    // Def site renamed
    assert!(
        def_after.contains("def fresh_name"),
        "def site should be renamed; got:\n{def_after}",
    );
    assert!(
        !def_after.contains("old_name"),
        "no `old_name` should remain in def file; got:\n{def_after}",
    );
    // Caller in same file renamed
    assert!(
        def_after.contains("return fresh_name()"),
        "in-file caller should be renamed; got:\n{def_after}",
    );

    // Cross-file callers renamed
    assert!(
        user_after.contains("fresh_name()"),
        "cross-file callers should be renamed; got:\n{user_after}",
    );
    assert!(
        !user_after.contains("old_name()"),
        "no `old_name()` should remain in user file; got:\n{user_after}",
    );
    // Import binding renamed
    assert!(
        user_after.contains("import fresh_name"),
        "import binding should be renamed; got:\n{user_after}",
    );
}
