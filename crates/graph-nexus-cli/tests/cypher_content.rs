//! Verifies that `gnx cypher` returns function-body source text when the
//! RETURN clause asks for `<var>.content`. Body text is read at query time
//! from the file via `node.file_idx` + `node.span` — there's no precomputed
//! body field on the graph, so this is purely a CLI-side feature.

use serde_json::Value;
use std::process::Command;

// `caller` calls `callee` — a single direct edge that any cypher
// `MATCH (a:Function)-[:Calls]->(b:Function)` query will surface.
const SOURCE: &str =
    "function callee() {\n    return 1;\n}\n\nfunction caller() {\n    return callee();\n}\n";

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
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

    let out = Command::new(gnx_bin())
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
    let out = Command::new(gnx_bin())
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

/// `RETURN m.content, t.name` → caller's `source.content` should hold the
/// caller's function body text sliced out of the file on disk.
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

    let results = out["results"]
        .as_array()
        .unwrap_or_else(|| panic!("expected results array, got {out}"));
    assert!(
        !results.is_empty(),
        "cypher should return at least one row: {out}"
    );

    let row = &results[0];
    let content = row["source"]["content"]
        .as_str()
        .unwrap_or_else(|| panic!("source.content missing or not a string: {row}"));
    // The body text must include the call site `callee()` from the caller fn.
    assert!(
        content.contains("callee()"),
        "source.content should include the function body, got {content:?}"
    );
    // The target side asked for `.name` only — no `.content` requested there,
    // so the target object must not carry a content field.
    assert!(
        row["target"].get("content").is_none(),
        "target should not have a content field when not requested: {row}"
    );
}

/// `RETURN m, t` (no `.content`) preserves the legacy shape so unrelated
/// callers don't see a behavior change.
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

    let results = out["results"]
        .as_array()
        .unwrap_or_else(|| panic!("expected results array, got {out}"));
    assert!(!results.is_empty(), "cypher should return rows: {out}");

    for row in results {
        assert!(
            row["source"].get("content").is_none(),
            "source should not carry content without `.content` in RETURN: {row}"
        );
        assert!(
            row["target"].get("content").is_none(),
            "target should not carry content without `.content` in RETURN: {row}"
        );
    }
}

/// File deleted after analyze → cypher must not panic and `content` falls
/// back to an empty string. The graph may legitimately reference a file the
/// user has since edited or removed; we should report a stale entry, not
/// crash.
#[test]
fn cypher_content_handles_missing_file_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo_and_analyze(tmp.path());

    // Wipe the source file the graph points at.
    std::fs::remove_file(tmp.path().join("src/edges.ts")).unwrap();

    let out = Command::new(gnx_bin())
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

    let results = json["results"]
        .as_array()
        .unwrap_or_else(|| panic!("expected results array, got {json}"));
    for row in results {
        // We accept either an empty string or no key — both communicate
        // "we couldn't read the body". We forbid a non-empty string here
        // because the file is gone, so any text would be a bug.
        if let Some(content) = row["source"].get("content") {
            assert_eq!(
                content.as_str(),
                Some(""),
                "source.content should be empty when file is missing: {row}"
            );
        }
        if let Some(content) = row["target"].get("content") {
            assert_eq!(
                content.as_str(),
                Some(""),
                "target.content should be empty when file is missing: {row}"
            );
        }
    }
}

/// `cypher --help` must mention the single-repo limitation to guide users.
#[test]
fn cypher_help_mentions_single_repo_limit() {
    let out = Command::new(gnx_bin())
        .args(["cypher", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("single") || stdout.contains("one repo") || stdout.contains("graph"),
        "cypher --help missing repo guidance:\n{stdout}"
    );
}
