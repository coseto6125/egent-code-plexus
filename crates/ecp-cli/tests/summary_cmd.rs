//! Integration tests for `ecp summary`.
//!
//! Tests validate:
//!   1. Without `--repo`: registry-level overview (indexed_repos + groups).
//!   2. With `--repo .`: per-repo health sections present.
//!   3. With `--repo @test-group`: graceful handling (no crash).
//!
//! The registry is built from a temp HOME so tests are isolated from the
//! developer's real ~/.ecp registry.

use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Run `ecp summary [args]` with a synthetic HOME (empty registry) and
/// return stdout as a String.
fn run_summary_empty_registry(extra: &[&str]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .args(["summary"])
        .args(extra)
        .env("HOME", tmp.path())
        .env("ECP_HOME", tmp.path().join(".ecp"))
        .output()
        .expect("summary failed to spawn");
    assert!(
        out.status.success(),
        "summary exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `ecp summary [args]` against a registry that has one registered repo.
/// The repo is a real temp dir indexed via `ecp admin index`. Indexing now
/// upserts the global registry (the only writer that does — `admin group`
/// requires a pre-existing entry, `admin register` doesn't exist as a
/// subcommand), so this is the canonical setup path.
fn run_summary_with_registered_repo(extra: &[&str]) -> (String, tempfile::TempDir) {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    init_git_repo(repo_tmp.path());

    let idx_out = Command::new(ecp_bin())
        .args([
            "admin",
            "index",
            "--repo",
            repo_tmp.path().to_str().unwrap(),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .expect("admin index failed to spawn");
    assert!(
        idx_out.status.success(),
        "admin index exited non-zero: stderr={}",
        String::from_utf8_lossy(&idx_out.stderr)
    );

    let out = Command::new(ecp_bin())
        .args(["summary"])
        .args(extra)
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("summary failed to spawn");
    assert!(
        out.status.success(),
        "summary exited non-zero: stderr={}",
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

/// Without `--repo`, summary must emit a registry-level overview that
/// includes the `indexed_repos` and `groups` keys.
#[test]
fn summary_without_repo_lists_registry() {
    let stdout = run_summary_empty_registry(&["--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("summary --format json is not valid JSON: {e}\nstdout: {stdout}")
    });

    let summary = &v["summary"];
    assert!(
        summary.is_object(),
        "expected summary object in output:\n{stdout}"
    );
    assert!(
        summary.get("indexed_repos").is_some(),
        "missing indexed_repos key:\n{stdout}"
    );
    assert!(
        summary.get("groups").is_some(),
        "missing groups key:\n{stdout}"
    );
    // No per_repo when --repo omitted.
    assert!(
        summary.get("per_repo").is_none(),
        "unexpected per_repo when no --repo given:\n{stdout}"
    );
}

/// Default toon format must not panic; output should be non-empty.
#[test]
fn summary_default_format_succeeds() {
    let stdout = run_summary_empty_registry(&[]);
    assert!(
        !stdout.is_empty(),
        "summary produced no output in toon format"
    );
}

/// With `--repo .` pointing to a registered repo, per-repo health sections
/// (frameworks, freshness, blind_spots) must be present. External-client
/// usage (HTTP/DB/Redis/queue) is intentionally NOT a summary section —
/// see the standalone `ecp tool-map` command.
#[test]
fn summary_with_repo_includes_health_sections() {
    let (stdout, _home) = run_summary_with_registered_repo(&["--format", "json", "--repo", "."]);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("summary --format json is not valid JSON: {e}\nstdout: {stdout}")
    });

    let per_repo = v["summary"]["per_repo"]
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
        "externals_summary should NOT be a summary section (use `ecp tool-map`)"
    );
}

/// `--repo @unknown-group` must not panic. An unknown group returns exit 1
/// with an error message (not a panic) per the current selector behaviour.
#[test]
fn summary_at_group_unknown_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .args(["summary", "--repo", "@unknown-group", "--format", "json"])
        .env("HOME", tmp.path())
        .output()
        .expect("summary failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("thread") || !stderr.contains("panic"),
        "summary panicked on unknown group:\n{stderr}"
    );
}

/// Verify the `--help` output surfaces the command with its flags.
#[test]
fn summary_help_output() {
    let out = Command::new(ecp_bin())
        .args(["summary", "--help"])
        .output()
        .expect("summary --help failed to spawn");
    assert!(out.status.success(), "summary --help exited non-zero");
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
