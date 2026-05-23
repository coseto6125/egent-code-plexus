//! Pin the contract that `ecp find` exposes how many candidates were
//! considered, not just the ones returned.
//!
//! Without `--all`, the default `exact`/`fuzzy` mode applies
//! `candidates.into_iter().take(1)` and emits a single match. The LLM that
//! consumes the output must be able to tell whether `matches.len() == 1`
//! means "one candidate existed" or "one of N was picked" — otherwise it
//! will treat the absence of other rows as the absence of other symbols
//! and miss legitimate duplicates during rename / impact analysis.
//!
//! Contract: payload always carries `total_candidates` and `returned`, and
//! `returned <= total_candidates`. The `--all` flag and the no-match path
//! share the same code path so a single truncation-mode test is enough to
//! pin the field's presence — Serialize + the matching invariant guard
//! cover the other shapes.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn find_default_truncation_surfaces_omitted_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // 3 distinct files each defining the same top-level name. Default
    // exact-mode find picks one but the payload must expose the other two.
    write(repo, "src/a.rs", "pub struct Widget;\n");
    write(repo, "src/b.rs", "pub struct Widget;\n");
    write(repo, "src/c.rs", "pub struct Widget;\n");
    init_and_analyze(repo);

    let out = Command::new(ecp_bin())
        .args(["find", "Widget", "--format", "json", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp find failed to spawn");
    assert!(
        out.status.success(),
        "ecp find failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result: Value =
        serde_json::from_slice(&out.stdout).expect("ecp find produced non-JSON output");

    let total = result["total_candidates"]
        .as_u64()
        .expect("payload must carry `total_candidates`");
    let returned = result["returned"]
        .as_u64()
        .expect("payload must carry `returned`");
    let matches_len = result["matches"]
        .as_array()
        .map(|a| a.len() as u64)
        .expect("payload must carry `matches`");

    assert_eq!(
        matches_len, returned,
        "`returned` must equal matches.len(): result={result}"
    );
    assert!(
        returned <= total,
        "returned ({returned}) must not exceed total_candidates ({total})"
    );
    assert_eq!(
        total, 3,
        "expected 3 candidates across a.rs/b.rs/c.rs, got total={total}: {result}"
    );
    assert_eq!(
        returned, 1,
        "default mode should return 1, got returned={returned}: {result}"
    );
}
