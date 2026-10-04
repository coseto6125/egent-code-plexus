//! FU-2026-05-29-011: `ecp impact` on a high-collision target must self-flag
//! that its caller set may be incomplete. Same-named definitions mean bare
//! calls (no import/qualifier context) were ambiguity-suppressed at index
//! time (Tier-3 `AmbiguousGlobal`) — the upstream BFS can't see them, and a
//! silent low caller count reads as "safe to refactor" when it isn't.

mod common;

use common::{ecp_bin, run_git};
use std::path::Path;
use std::process::Command;

/// Two same-named `process` defs in different files, a bare caller that the
/// Tier-3 defence suppresses, and one collision-free symbol as control.
fn setup_collision_repo(repo: &Path, home: &Path) {
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/alpha.py"), "def process():\n    return 1\n").unwrap();
    std::fs::write(repo.join("src/beta.py"), "def process():\n    return 2\n").unwrap();
    std::fs::write(
        repo.join("src/caller.py"),
        "def run_all():\n    process()\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/unique.py"),
        "def unique_entry():\n    return 3\n\ndef call_unique():\n    unique_entry()\n",
    )
    .unwrap();
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/ambiguity-test.git",
        ],
    );
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_impact(repo: &Path, home: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["impact"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--repo", ".", "--format", "json"]);
    let out = Command::new(ecp_bin())
        .args(&args)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("impact failed to spawn");
    assert!(
        out.status.success(),
        "impact {extra:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn impact_on_collision_name_flags_incomplete_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // --kind narrows past the ambiguous-target error; the graph still holds
    // 2 same-named defs, so the upstream caller set must self-flag.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--kind", "function", "--direction", "up"],
    );
    let caveat = json
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("collision target must carry a `result` caveat: {json}"));
    assert!(
        caveat.contains("incomplete"),
        "caveat must say the caller set may be incomplete: {caveat}"
    );
    assert!(
        caveat.contains("2 same-named"),
        "caveat must count the same-named definitions: {caveat}"
    );
    // The contract: the caveat names a command the agent can run as-is,
    // carrying the --kind that made the colliding name resolvable.
    assert!(
        caveat.contains("ecp impact --target process --kind function --ambiguous-callers"),
        "caveat must name the runnable command: {caveat}"
    );
    assert!(
        json.get("ambiguous_callers").is_none(),
        "without the flag the payload must not carry the sites: {json}"
    );
}

#[test]
fn test_ambiguous_callers_json_format_parses_and_lists_suppressed_bare_call() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // run_impact parses stdout as JSON, so a parse failure fails this test.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--kind", "function", "--ambiguous-callers"],
    );
    let field = &json["ambiguous_callers"];
    let sites = field["sites"]
        .as_array()
        .unwrap_or_else(|| panic!("ambiguous_callers.sites expected: {json}"));
    assert!(
        sites.iter().any(|s| s["file"] == "src/caller.py"
            && s["line"] == 2
            && s["enclosing"] == "run_all"
            && s["form"] == "bare"),
        "the suppressed bare call in run_all must be listed: {json}"
    );
    assert!(
        sites
            .iter()
            .all(|s| s["file"] != "src/alpha.py" && s["file"] != "src/beta.py"),
        "definition lines must be dropped: {json}"
    );
    assert_eq!(field["shown"].as_u64(), Some(sites.len() as u64));
    let caveat = json["result"].as_str().unwrap_or_default();
    assert!(
        caveat.contains("`ambiguous_callers` field"),
        "with the flag set the caveat points at the field: {caveat}"
    );
}

#[test]
fn test_ambiguous_callers_batch_targets_each_get_own_field() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    let mut child = Command::new(ecp_bin())
        .args([
            "impact",
            "--batch",
            "--kind",
            "function",
            "--ambiguous-callers",
            "--repo",
            ".",
            "--format",
            "json",
        ])
        .current_dir(repo)
        .env("HOME", home_tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("impact --batch failed to spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"process\nunique_entry\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let blocks: Vec<(String, serde_json::Value)> = stdout
        .split("=== target: ")
        .filter(|b| !b.trim().is_empty())
        .map(|b| {
            let (name, body) = b.split_once(" ===\n").expect("block header");
            (name.to_string(), serde_json::from_str(body.trim()).unwrap())
        })
        .collect();
    assert_eq!(blocks.len(), 2, "one block per target: {stdout}");
    assert!(
        blocks[0].1["ambiguous_callers"]["sites"].is_array(),
        "colliding target carries its own sites: {stdout}"
    );
    assert!(
        blocks[1].1.get("ambiguous_callers").is_none(),
        "collision-free target carries no field: {stdout}"
    );
}

#[test]
fn impact_on_unique_name_stays_caveat_free() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    let json = run_impact(
        repo,
        home_tmp.path(),
        &["unique_entry", "--direction", "up"],
    );
    assert!(
        json.get("result").is_none(),
        "collision-free target must not pay the caveat token cost: {json}"
    );
}

#[test]
fn downstream_impact_on_collision_name_stays_caveat_free() {
    let tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    setup_collision_repo(repo, home_tmp.path());

    // The suppressed edges are *incoming* bare calls; a downstream (callee)
    // walk of the target is unaffected, so no caveat.
    let json = run_impact(
        repo,
        home_tmp.path(),
        &["process", "--kind", "function", "--direction", "down"],
    );
    assert!(
        json.get("result").is_none(),
        "downstream walk must not carry the caller-set caveat: {json}"
    );
}
