//! Integration tests for `ecp contracts`.
//!
//! v1 scope: structural shape + error gate + flag surface.
//! A real 2-repo happy-path fixture is left as an #[ignore] skeleton.

use std::process::Command;

fn ecp() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ecp"))
}

// ── Gate: single / zero repos ────────────────────────────────────────────────

/// Stands for: `contracts` on a directory that is not a git repo gives a
/// non-zero exit with a reason, never an empty success. The command used to
/// demand two or more repos; since P8c one repo works, so the fixture is a
/// plain temp dir instead of the test binary's own crate directory.
#[test]
fn contracts_unindexable_repo_errors() {
    let home_tmp = tempfile::tempdir().unwrap();
    let work_tmp = tempfile::tempdir().unwrap();
    let output = ecp()
        .args(["contracts", "--repo", "."])
        .current_dir(work_tmp.path())
        .env("HOME", home_tmp.path())
        .env_remove("ECP_HOME")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit for a non-git directory"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !combined.trim().is_empty(),
        "expected an error message, got nothing"
    );
}

// ── Help flag surface ─────────────────────────────────────────────────────────

#[test]
fn contracts_help_includes_kind_flag() {
    let output = ecp().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "ecp contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--kind"),
        "--kind missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_repo_flag() {
    let output = ecp().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "ecp contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--repo"),
        "--repo missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_unmatched_only_flag() {
    let output = ecp().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "ecp contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--unmatched-only") || stdout.contains("--unmatched_only"),
        "--unmatched-only missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_format_flag() {
    let output = ecp().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "ecp contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--format"),
        "--format missing from contracts --help:\n{stdout}"
    );
}

// ── Multi-repo happy path (skeleton — requires fixture setup) ─────────────────

/// Full happy-path: two registered repos → command succeeds and returns
/// a payload with `repos_scanned`, `pairs`, `unmatched_producer_count`,
/// and `unmatched_consumer_count`.
///
/// Skipped until a 2-repo fixture is wired; add the fixture and remove
/// #[ignore] when implementing the extraction phase.
#[test]
#[ignore = "requires 2-repo registry fixture — implement in contracts extraction follow-up"]
fn contracts_two_repos_returns_shape() {
    // TODO: spin up two temp repos in the registry, run:
    //   ecp contracts --repo @fixture-group --format json
    // then assert the JSON contains repos_scanned=2, pairs=[], etc.
    todo!("wire 2-repo fixture");
}

/// --unmatched-only with two repos should return unmatched_producers and
/// unmatched_consumers arrays (even if both empty in the stub phase).
#[test]
#[ignore = "requires 2-repo registry fixture — implement in contracts extraction follow-up"]
fn contracts_unmatched_only_returns_shape() {
    todo!("wire 2-repo fixture");
}
