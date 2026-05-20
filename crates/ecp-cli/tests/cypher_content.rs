//! Verifies that `ecp cypher` returns function-body source text when the
//! RETURN clause asks for `<var>.content`. Body text is read at query time
//! from the file via `node.file_idx` + `node.span` — there's no precomputed
//! body field on the graph, so this is purely a CLI-side feature.

use serde_json::Value;
use std::process::Command;

// `caller` calls `callee` — a single direct edge that any cypher
// `MATCH (a:Function)-[:Calls]->(b:Function)` query will surface.
const SOURCE: &str =
    "function callee() {\n    return 1;\n}\n\nfunction caller() {\n    return callee();\n}\n";

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

/// `RETURN m.content, t.name` → the content column should hold the caller's
/// function body text sliced out of the file on disk.
#[test]
fn cypher_returns_node_content_when_requested() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (m:Function)-[r:Calls]->(t:Function) WHERE m.name='caller' RETURN m.content, t.name",
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
        "cypher should return at least one row: {out}"
    );

    // Locate the column indices.
    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    let m_content_col = col_names
        .iter()
        .position(|&c| c == "m.content")
        .unwrap_or_else(|| panic!("expected column m.content in {col_names:?}"));
    let t_name_col = col_names
        .iter()
        .position(|&c| c == "t.name")
        .unwrap_or_else(|| panic!("expected column t.name in {col_names:?}"));

    let row = &rows[0];
    let content = row[m_content_col]
        .as_str()
        .unwrap_or_else(|| panic!("m.content cell missing or not a string: {row}"));
    // The body text must include the call site `callee()` from the caller fn.
    assert!(
        content.contains("callee()"),
        "m.content should include the function body, got {content:?}"
    );

    // t.name column should exist and be a non-empty string.
    assert!(
        rows[0][t_name_col].is_string(),
        "t.name should be a string: {row}"
    );

    // No m.content column requested for t — verify no "t.content" column.
    assert!(
        !col_names.contains(&"t.content"),
        "t.content should not be in columns when not requested: {col_names:?}"
    );
}

/// `RETURN m, t` (bare vars) — Phase C9 expands each into 3 columns:
/// `<var>.name, <var>.kind, <var>.filePath`. Verify NO `m.content` or
/// `t.content` column appears (content is only present when explicitly asked).
#[test]
fn cypher_without_content_returns_only_name() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    let out = run_json(
        tmp.path(),
        &[
            "cypher",
            "MATCH (m:Function)-[r:Calls]->(t:Function) RETURN m, t",
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
    assert!(!rows.is_empty(), "cypher should return rows: {out}");

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();

    // Bare `m` expands to m.name, m.kind, m.filePath — never m.content.
    assert!(
        !col_names.contains(&"m.content"),
        "m.content should not be in columns without `.content` in RETURN: {col_names:?}"
    );
    assert!(
        !col_names.contains(&"t.content"),
        "t.content should not be in columns without `.content` in RETURN: {col_names:?}"
    );
}

/// File deleted after analyze → cypher must not panic and `.content` columns
/// fall back to empty string. The graph may legitimately reference a file the
/// user has since edited or removed; we should report a stale entry, not crash.
#[test]
fn cypher_content_handles_missing_file_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Wipe the source file the graph points at.
    std::fs::remove_file(tmp.path().join("src/edges.ts")).unwrap();

    let out = Command::new(ecp_bin())
        .args([
            "cypher",
            "MATCH (m:Function)-[r:Calls]->(t:Function) RETURN m.content, t.content",
            "--format",
            "json",
        ])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .output()
        .expect("cypher failed to spawn");
    assert!(
        out.status.success(),
        "cypher should exit 0 even when source files are missing: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("cypher did not return JSON: {stdout}"));
    let json: Value = serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|err| panic!("cypher did not return JSON: {err}\nstdout={stdout}"));

    let columns = json["columns"]
        .as_array()
        .unwrap_or_else(|| panic!("expected columns array, got {json}"));
    let rows = json["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("expected rows array, got {json}"));

    let col_names: Vec<&str> = columns.iter().map(|c| c.as_str().unwrap()).collect();
    let m_content_col = col_names.iter().position(|&c| c == "m.content");
    let t_content_col = col_names.iter().position(|&c| c == "t.content");

    for row in rows {
        // When file is missing, content must be empty string — not a non-empty body.
        if let Some(idx) = m_content_col {
            let cell = &row[idx];
            assert_eq!(
                cell.as_str(),
                Some(""),
                "m.content should be empty when file is missing: {row}"
            );
        }
        if let Some(idx) = t_content_col {
            let cell = &row[idx];
            assert_eq!(
                cell.as_str(),
                Some(""),
                "t.content should be empty when file is missing: {row}"
            );
        }
    }
}

/// `cypher --help` must mention the single-repo limitation to guide users.
#[test]
fn cypher_help_mentions_single_repo_limit() {
    let out = Command::new(ecp_bin())
        .args(["cypher", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("single") || stdout.contains("one repo") || stdout.contains("graph"),
        "cypher --help missing repo guidance:\n{stdout}"
    );
}
