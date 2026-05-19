//! E2E tests for `cgn cypher --format toon` output.

use std::process::Command;

// caller -> callee: one Calls edge.
const SOURCE: &str = r#"
function callee() { return 1; }
function caller() { return callee(); }
"#;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/toon.ts"), SOURCE).unwrap();

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

    let out = Command::new(cgn_bin())
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

fn run_toon(repo: &std::path::Path, query: &str) -> String {
    let out = Command::new(cgn_bin())
        .args(["cypher", query, "--format", "toon"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "cypher --format toon failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Basic toon output: etoon-encoded columns + rows header + cell values.
#[test]
fn toon_output_basic() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let stdout = run_toon(
        tmp.path(),
        "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name",
    );

    // etoon encodes the column names somewhere in the output (e.g. `a.name,b.name`).
    assert!(
        stdout.contains("a.name") && stdout.contains("b.name"),
        "toon output missing column names:\n{stdout}"
    );
    // One result row means the row-count indicator contains "1".
    assert!(
        stdout.contains("rows[1]"),
        "toon output should report rows[1] (one call edge):\n{stdout}"
    );
    // The row should contain the caller and callee names.
    assert!(
        stdout.contains("caller") && stdout.contains("callee"),
        "toon row should contain caller/callee names:\n{stdout}"
    );
}

/// COUNT(*) query → toon should show `rows[1]:` and a numeric cell.
#[test]
fn toon_output_with_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let stdout = run_toon(
        tmp.path(),
        "MATCH (a:Function)-[:Calls]->(b:Function) RETURN a.name, COUNT(*) AS n",
    );

    assert!(
        stdout.contains("rows[1]:"),
        "toon output should have rows[1] for one caller group:\n{stdout}"
    );
    // The numeric count cell must appear somewhere in the row output.
    assert!(
        stdout.contains('1'),
        "toon row should contain numeric count (1):\n{stdout}"
    );
}
