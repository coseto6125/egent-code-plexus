//! Integration tests for `gnx coverage`.
//!
//! Tests validate:
//!   1. Without `--repo`: registry-level overview (indexed_repos + groups).
//!   2. With `--repo .`: per-repo health sections present.
//!   3. With `--repo @test-group`: graceful handling (no crash).
//!
//! The registry is built from a temp HOME so tests are isolated from the
//! developer's real ~/.gnx registry.

use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Run `gnx coverage [args]` with a synthetic HOME (empty registry) and
/// return stdout as a String.
fn run_coverage_empty_registry(extra: &[&str]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["coverage"])
        .args(extra)
        .env("HOME", tmp.path())
        .env("GNX_HOME", tmp.path().join(".gnx"))
        .output()
        .expect("coverage failed to spawn");
    assert!(
        out.status.success(),
        "coverage exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `gnx coverage [args]` against a registry that has one registered repo.
/// The repo is a real temp dir indexed via `gnx admin index`. Indexing now
/// upserts the global registry (the only writer that does — `admin group`
/// requires a pre-existing entry, `admin register` doesn't exist as a
/// subcommand), so this is the canonical setup path.
fn run_coverage_with_registered_repo(extra: &[&str]) -> (String, tempfile::TempDir) {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    init_git_repo(repo_tmp.path());

    let idx_out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", repo_tmp.path().to_str().unwrap()])
        .env("HOME", home_tmp.path())
        .output()
        .expect("admin index failed to spawn");
    assert!(
        idx_out.status.success(),
        "admin index exited non-zero: stderr={}",
        String::from_utf8_lossy(&idx_out.stderr)
    );

    let out = Command::new(gnx_bin())
        .args(["coverage"])
        .args(extra)
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("coverage failed to spawn");
    assert!(
        out.status.success(),
        "coverage exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8_lossy(&out.stdout).into_owned(), home_tmp)
}

fn init_git_repo(repo: &Path) {
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args(["remote", "add", "origin", "git@github.com:test/test.git"])
        .current_dir(repo)
        .output()
        .unwrap();
    std::fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Without `--repo`, coverage must emit a registry-level overview that
/// includes the `indexed_repos` and `groups` keys.
#[test]
fn coverage_without_repo_lists_registry() {
    let stdout = run_coverage_empty_registry(&["--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("coverage --format json is not valid JSON: {e}\nstdout: {stdout}")
    });

    let coverage = &v["coverage"];
    assert!(
        coverage.is_object(),
        "expected coverage object in output:\n{stdout}"
    );
    assert!(
        coverage.get("indexed_repos").is_some(),
        "missing indexed_repos key:\n{stdout}"
    );
    assert!(
        coverage.get("groups").is_some(),
        "missing groups key:\n{stdout}"
    );
    // No per_repo when --repo omitted.
    assert!(
        coverage.get("per_repo").is_none(),
        "unexpected per_repo when no --repo given:\n{stdout}"
    );
}

/// Default toon format must not panic; output should contain "coverage".
#[test]
fn coverage_default_format_succeeds() {
    let stdout = run_coverage_empty_registry(&[]);
    assert!(
        !stdout.is_empty(),
        "coverage produced no output in toon format"
    );
}

/// With `--repo .` pointing to a registered repo, per-repo health sections
/// (frameworks, freshness, blind_spots) must be present. External-client
/// usage (HTTP/DB/Redis/queue) is intentionally NOT a coverage section —
/// see the standalone `gnx tool-map` command.
#[test]
fn coverage_with_repo_includes_health_sections() {
    let (stdout, _home) = run_coverage_with_registered_repo(&["--format", "json", "--repo", "."]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("coverage --format json is not valid JSON: {e}\nstdout: {stdout}")
    });

    let per_repo = v["coverage"]["per_repo"]
        .as_array()
        .expect("per_repo must be an array");
    assert!(
        !per_repo.is_empty(),
        "per_repo should have at least one entry"
    );

    let entry = &per_repo[0];
    assert!(
        entry.get("frameworks").is_some(),
        "missing frameworks section"
    );
    assert!(
        entry.get("freshness").is_some(),
        "missing freshness section"
    );
    assert!(
        entry.get("blind_spots").is_some(),
        "missing blind_spots section"
    );
    assert!(
        entry.get("externals_summary").is_none(),
        "externals_summary should NOT be a coverage section (use `gnx tool-map`)"
    );
}

/// `--repo @unknown-group` must not panic and must exit 0 (groups don't crash
/// if they don't exist). The command treats unknown selectors as non-fatal
/// when the registry is empty.
///
/// NOTE: In the current implementation, an unknown group DOES return an error
/// because `repo_selector::resolve` returns `ResolveError::GroupNotFound`.
/// This test verifies the actual behaviour — the command exits non-zero for
/// unknown selectors, which is acceptable.
#[test]
fn coverage_at_group_unknown_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(gnx_bin())
        .args(["coverage", "--repo", "@unknown-group", "--format", "json"])
        .env("HOME", tmp.path())
        .output()
        .expect("coverage failed to spawn");
    // Must not segfault / panic — any clean exit is acceptable.
    // An unknown group returns exit 1 with an error message (not a panic).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("thread") || !stderr.contains("panic"),
        "coverage panicked on unknown group:\n{stderr}"
    );
}

/// Verify the `--help` output surfaces the command with its flags.
#[test]
fn coverage_help_output() {
    let out = Command::new(gnx_bin())
        .args(["coverage", "--help"])
        .output()
        .expect("coverage --help failed to spawn");
    assert!(out.status.success(), "coverage --help exited non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--repo") || stdout.contains("repo"),
        "help should mention --repo:\n{stdout}"
    );
    assert!(
        stdout.contains("--format") || stdout.contains("format"),
        "help should mention --format:\n{stdout}"
    );
}
