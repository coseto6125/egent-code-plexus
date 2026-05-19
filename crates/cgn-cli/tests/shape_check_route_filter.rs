//! Verify `cgn shape-check --route <path>` filters Fetches edges
//! by target Route path. No-match case prints helpful message.
//!
//! Hermetic: builds a tempdir fixture and uses `HOME=<tempdir>` so the
//! registry + graph live entirely inside the temp directory. This avoids
//! race conditions with concurrent background reindex of the production
//! graph (which can leave `graph.bin` in a half-written state and crash
//! tests with `subtree pointer overran range` style errors).

use std::process::Command;
use tempfile::TempDir;

const FIXTURE_FETCH_CONSUMER: &str = r#"
async function loadUsers() {
    const r = await fetch('/api/users');
    return await r.json();
}
"#;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Build a tempdir with one TypeScript file containing a fetch() consumer,
/// initialize git, run `cgn admin index` with isolated HOME so the registry
/// is per-test. TempDir drop cleans up.
fn setup_fixture() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed");

    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/consumer.ts"), FIXTURE_FETCH_CONSUMER)
        .expect("write fixture");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .expect("git add");
    assert!(out.status.success(), "git add failed");

    let out = Command::new("git")
        .args([
            "-c", "user.email=t@t",
            "-c", "user.name=t",
            "commit", "-q", "-m", "init",
        ])
        .current_dir(repo)
        .output()
        .expect("git commit");
    assert!(out.status.success(), "git commit failed: stderr={}",
        String::from_utf8_lossy(&out.stderr));

    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    tmp
}

#[test]
fn shape_check_help_lists_route_arg() {
    // Help text doesn't depend on graph state; no fixture needed.
    let output = Command::new(gnx_bin())
        .args(["shape-check", "--help"])
        .output()
        .expect("run cgn shape-check --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--route"),
        "expected --route arg in help, got: {stdout}"
    );
}

#[test]
fn shape_check_route_no_match_emits_helpful_message() {
    let tmp = setup_fixture();
    let repo = tmp.path();

    let output = Command::new(gnx_bin())
        .args(["shape-check", "--route", "/__nonexistent_route__"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run cgn shape-check --route");

    assert!(
        output.status.success(),
        "shape-check should succeed on no-match graph; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No routes match"),
        "expected `No routes match` in stderr, got stderr: {stderr}"
    );
}
