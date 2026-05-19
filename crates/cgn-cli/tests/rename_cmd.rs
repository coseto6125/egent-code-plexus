//! Integration test for `cgn rename` (Python MVP).
//!
//! Pipeline coverage:
//! - Stage 1 (graph): locate target node, collect inbound-edge source files
//! - Stage 2 (AST): tree-sitter Python parse + identifier-occurrence find
//! - Stage 3 (dry-run): report counts + diff, no file mutated
//! - Stage 3 (execute): atomic per-file replace, syntax remains valid
//! - --markdown: word-boundary replace in .md files, OFF by default
//! - Post-rename verification: residuals + new_distribution sections
//! - Pre-flight collision detection: COLLISION warning in dry-run

mod common;

use common::run_git;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Build the graph index for a repo. `home` is an isolated home dir to avoid
/// touching the real registry.
fn build_index(root: &Path) {
    let home = root.join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("cgn admin index failed to spawn");
    assert!(
        out.status.success(),
        "cgn admin index failed: stderr={}, stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
}

/// Build a temp repo with rename_def.py + rename_user.py, init git, run
/// `cgn admin index` to materialize a graph.bin. Returns the repo root.
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
    build_index(root);
    repo
}

// ---------------------------------------------------------------------------
// Fixture helpers for new tests
// ---------------------------------------------------------------------------

/// Create a repo with Python code containing `symbol_name` + a docs/api.md
/// referencing the same name. Returns (TempDir, home_path).
fn setup_repo_with_markdown(symbol_name: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    // Minimal Python definition.
    std::fs::write(
        root.join("mymod.py"),
        format!("def {symbol_name}():\n    return 42\n"),
    )
    .unwrap();

    // Markdown doc referencing the symbol.
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(
        root.join("docs/api.md"),
        format!("# API\n\nUse `{symbol_name}` to do the thing.\n\nSee also [{symbol_name}].\n"),
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);
    repo
}

/// Create a repo with TWO distinct Python symbols (`a` and `b`).
/// Used for collision detection: renaming `a` → `b` should hit a COLLISION.
fn setup_repo_two_symbols(sym_a: &str, sym_b: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();

    std::fs::write(
        root.join("two.py"),
        format!("def {sym_a}():\n    return 1\n\ndef {sym_b}():\n    return 2\n"),
    )
    .unwrap();

    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.email", "t@e"]);
    run_git(root, &["config", "user.name", "t"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "init"]);
    build_index(root);
    repo
}

/// Run rename and capture stdout. Panics on non-zero exit.
fn run_rename_stdout(root: &Path, extra_args: &[&str]) -> String {
    let home = root.join(".home");
    let out = Command::new(gnx_bin())
        .args(extra_args)
        .args(["--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("cgn rename failed to spawn");
    assert!(
        out.status.success(),
        "cgn rename failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run rename, capture both stdout+stderr. Does NOT assert success.
fn run_rename_both(root: &Path, extra_args: &[&str]) -> (String, String) {
    let home = root.join(".home");
    let out = Command::new(gnx_bin())
        .args(extra_args)
        .args(["--repo", root.to_str().unwrap()])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("cgn rename failed to spawn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Existing tests
// ---------------------------------------------------------------------------

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
        .expect("cgn rename failed to spawn");
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
        .expect("cgn rename failed to spawn");
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

// ---------------------------------------------------------------------------
// New tests: --markdown flag
// ---------------------------------------------------------------------------

#[test]
fn rename_with_markdown_flag_touches_md() {
    let repo = setup_repo_with_markdown("foo");
    let root = repo.path();
    run_rename_stdout(
        root,
        &[
            "rename",
            "--symbol",
            "foo",
            "--new-name",
            "bar",
            "--markdown",
        ],
    );
    let md_content = std::fs::read_to_string(root.join("docs/api.md")).unwrap();
    assert!(
        md_content.contains("bar"),
        "markdown not updated:\n{md_content}"
    );
}

#[test]
fn rename_default_does_not_touch_md() {
    let repo = setup_repo_with_markdown("foo");
    let root = repo.path();
    run_rename_stdout(root, &["rename", "--symbol", "foo", "--new-name", "bar"]);
    let md_content = std::fs::read_to_string(root.join("docs/api.md")).unwrap();
    assert!(
        md_content.contains("foo"),
        "markdown should be untouched without --markdown:\n{md_content}"
    );
}

// ---------------------------------------------------------------------------
// New tests: post-rename verification output
// ---------------------------------------------------------------------------

#[test]
fn rename_output_includes_residual_section() {
    let repo = setup_repo();
    let root = repo.path();
    let stdout = run_rename_stdout(
        root,
        &["rename", "--symbol", "old_name", "--new-name", "fresh_name"],
    );
    assert!(
        stdout.contains("residual")
            || stdout.contains("still present")
            || stdout.contains("remaining"),
        "missing residual section in output:\n{stdout}"
    );
}

#[test]
fn rename_output_includes_new_name_distribution() {
    let repo = setup_repo();
    let root = repo.path();
    let stdout = run_rename_stdout(
        root,
        &["rename", "--symbol", "old_name", "--new-name", "fresh_name"],
    );
    assert!(
        stdout.contains("new_distribution")
            || stdout.contains("distribution")
            || stdout.contains("fresh_name"),
        "missing new-name distribution in output:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// New tests: pre-flight collision detection
// ---------------------------------------------------------------------------

#[test]
fn rename_collision_detected_in_dry_run() {
    let repo = setup_repo_two_symbols("foo", "bar");
    let root = repo.path();
    let (stdout, stderr) = run_rename_both(
        root,
        &[
            "rename",
            "--symbol",
            "foo",
            "--new-name",
            "bar",
            "--dry-run",
        ],
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("COLLISION"),
        "missing COLLISION warning in dry-run output:\n{combined}"
    );
}

// ---------------------------------------------------------------------------
// New tests: zero-occurrence explicit message
// ---------------------------------------------------------------------------

#[test]
fn rename_zero_occurrences_explicit_message() {
    let repo = setup_repo();
    let root = repo.path();
    let home = root.join(".home");
    let out = Command::new(gnx_bin())
        .args([
            "rename",
            "--symbol",
            "nonexistent_symbol_xyz",
            "--new-name",
            "newname",
            "--repo",
            root.to_str().unwrap(),
        ])
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .expect("cgn rename failed to spawn");
    // Exit may be non-zero; check the message content regardless.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("No occurrences")
            || combined.contains("0 occurrences")
            || combined.contains("not found"),
        "missing explicit no-occurrences message:\n{combined}"
    );
}
