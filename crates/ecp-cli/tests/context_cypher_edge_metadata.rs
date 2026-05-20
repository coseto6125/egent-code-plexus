//! Verifies that `ecp inspect` and `ecp cypher` surface per-edge metadata
//! (`reason`, `confidence`) so an LLM can distinguish direct AST calls from
//! lower-trust resolutions (reflection fanout, framework heuristics, etc.).

use serde_json::Value;
use std::process::Command;

// Two functions with a single direct call edge: `caller` -> `callee`.
const SOURCE: &str = r#"
function callee() {
    return 1;
}

function caller() {
    return callee();
}
"#;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn init_repo_and_analyze(repo: &std::path::Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/edges.ts"), SOURCE).unwrap();

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

fn run_json(repo: &std::path::Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("command failed to spawn");
    assert!(
        out.status.success(),
        "{args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("{args:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("{args:?} did not return JSON: {err}\nstdout={stdout}"))
}

fn assert_edge_metadata(entry: &Value, ctx: &str) {
    let reason = entry
        .get("reason")
        .unwrap_or_else(|| panic!("missing reason in {ctx}: {entry}"));
    assert!(
        reason.is_string(),
        "reason should be a string in {ctx}: {entry}"
    );
    assert!(
        !reason.as_str().unwrap().is_empty(),
        "reason should be non-empty in {ctx}: {entry}"
    );

    let confidence = entry
        .get("confidence")
        .unwrap_or_else(|| panic!("missing confidence in {ctx}: {entry}"));
    let conf = confidence
        .as_f64()
        .unwrap_or_else(|| panic!("confidence should be a number in {ctx}: {entry}"));
    assert!(
        (0.0..=1.0).contains(&conf),
        "confidence should be in [0,1] in {ctx}: {entry} (got {conf})"
    );
}

#[test]
fn context_outgoing_and_incoming_edges_expose_reason_and_confidence() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // `caller` should have an outgoing `calls` edge to `callee`.
    let out = run_json(
        tmp.path(),
        &["inspect", "--name", "caller", "--format", "json"],
    );
    assert_eq!(out["status"], "found");

    let outgoing_calls = out["outgoing"]["calls"]
        .as_array()
        .unwrap_or_else(|| panic!("expected outgoing.calls array, got {out}"));
    assert!(
        !outgoing_calls.is_empty(),
        "caller should have at least one outgoing call edge: {out}"
    );
    for entry in outgoing_calls {
        assert_edge_metadata(entry, "inspect outgoing call");
    }

    // `callee` should have an incoming `calls` edge from `caller`.
    let in_out = run_json(
        tmp.path(),
        &["inspect", "--name", "callee", "--format", "json"],
    );
    assert_eq!(in_out["status"], "found");
    let incoming_calls = in_out["incoming"]["calls"]
        .as_array()
        .unwrap_or_else(|| panic!("expected incoming.calls array, got {in_out}"));
    assert!(
        !incoming_calls.is_empty(),
        "callee should have at least one incoming call edge: {in_out}"
    );
    for entry in incoming_calls {
        assert_edge_metadata(entry, "inspect incoming call");
    }
}

#[test]
fn cypher_direct_edge_results_expose_edge_reason_and_confidence() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (a:Function)-[r:Calls]->(b:Function) RETURN a.name, b.name, r.confidence, r.reason",
            "--format",
            "json",
        ],
    );

    let columns = out["columns"]
        .as_array()
        .unwrap_or_else(|| panic!("expected columns array, got {out}"));
    let rows = out["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("expected rows array, got {out}"));
    assert!(
        !rows.is_empty(),
        "cypher should return at least one direct call edge: {out}"
    );

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    for expected in ["a.name", "b.name", "r.confidence", "r.reason"] {
        assert!(
            col_names.contains(&expected),
            "expected column {expected} in {col_names:?}"
        );
    }

    let conf_col = col_names.iter().position(|&c| c == "r.confidence").unwrap();
    let reason_col = col_names.iter().position(|&c| c == "r.reason").unwrap();

    for row in rows {
        let conf = row[conf_col]
            .as_f64()
            .unwrap_or_else(|| panic!("r.confidence should be a number: {row}"));
        assert!(
            (0.0..=1.0).contains(&conf),
            "r.confidence should be in [0,1]: {conf} in {row}"
        );

        let reason = row[reason_col]
            .as_str()
            .unwrap_or_else(|| panic!("r.reason should be a string: {row}"));
        assert!(!reason.is_empty(), "r.reason should be non-empty: {row}");
    }
}
