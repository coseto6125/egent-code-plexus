//! Integration tests for `cgn contracts`.
//!
//! v1 scope: structural shape + multi-repo gate + flag surface.
//! A real 2-repo happy-path fixture is left as an #[ignore] skeleton.

use std::process::Command;

fn cgn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gnx"))
}

// ── Gate: single / zero repos ────────────────────────────────────────────────

/// With --repo . (path not registered → 0 repos or 1 repo), the command
/// should fail with a helpful message mentioning the cross-repo requirement.
#[test]
fn contracts_single_repo_errors() {
    let home_tmp = tempfile::tempdir().unwrap();
    let output = cgn()
        .args(["contracts", "--repo", "."])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();

    // Must exit non-zero.
    assert!(
        !output.status.success(),
        "expected non-zero exit for single-repo contracts"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");

    // Should contain a hint about the multi-repo requirement.
    assert!(
        combined.contains("≥2")
            || combined.contains("multi")
            || combined.contains("cross")
            || combined.contains("at least")
            || combined.contains("more than one")
            || combined.contains("not registered")
            || combined.contains("not in registry")
            || combined.contains("contracts requires"),
        "expected helpful error for single-repo / unregistered path:\n{combined}"
    );
}

// ── Help flag surface ─────────────────────────────────────────────────────────

#[test]
fn contracts_help_includes_kind_flag() {
    let output = cgn().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "cgn contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--kind"),
        "--kind missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_repo_flag() {
    let output = cgn().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "cgn contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--repo"),
        "--repo missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_unmatched_only_flag() {
    let output = cgn().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "cgn contracts --help exited non-zero"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--unmatched-only") || stdout.contains("--unmatched_only"),
        "--unmatched-only missing from contracts --help:\n{stdout}"
    );
}

#[test]
fn contracts_help_includes_format_flag() {
    let output = cgn().args(["contracts", "--help"]).output().unwrap();
    assert!(
        output.status.success(),
        "cgn contracts --help exited non-zero"
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
    //   cgn contracts --repo @fixture-group --format json
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
