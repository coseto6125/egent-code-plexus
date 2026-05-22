//! Integration tests for `ecp diff` A-mode snapshot diff.
//!
//! A-mode uses two pre-built `graph.bin` files (no git operations):
//! `ecp diff --section symbols --baseline-graph <path> --current-graph <path>`
//!
//! Restrictions:
//! - Sections allowed: routes, contracts, symbols (NOT bindings — needs resolver JSONL).
//! - Clap validation: `--baseline-graph` requires `--current-graph`.
//! - Output: `baseline_sha` and `current_sha` are empty strings (no git SHAs in A-mode).

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// v1: single function returning 42.
const V1_CODE: &str = r#"
function foo() {
  return 42;
}
"#;

/// v2: foo's body changed (return 99), new function bar added.
const V2_CODE: &str = r#"
function foo() {
  return 99;
}

function bar() {
  return 100;
}
"#;

/// Index the repo and return the path to graph.bin.
fn index_repo(repo_path: &std::path::Path) -> std::path::PathBuf {
    let output = Command::new(ecp_bin())
        .args([
            "admin",
            "index",
            "--repo",
            repo_path.to_string_lossy().as_ref(),
        ])
        .env("HOME", repo_path)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp admin index");
    assert!(
        output.status.success(),
        "ecp admin index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    find_graph_bin(repo_path)
}

/// Walk .ecp directory and return the graph.bin with the newest mtime.
/// `admin index` writes per-commit/per-content caches into nested subdirs
/// (`.ecp/<key>/graph.bin`); returning the first match would pick up a
/// stale cache from an earlier index call. readdir ordering is not
/// guaranteed across platforms — Linux CI orders differently from
/// dev-host macOS, which is why selecting by mtime is load-bearing.
fn find_graph_bin(repo_path: &std::path::Path) -> std::path::PathBuf {
    fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                out.push(p);
            } else if p.is_dir() {
                walk(&p, depth - 1, out);
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo_path.join(".ecp"), 5, &mut found);
    assert!(
        !found.is_empty(),
        "no graph.bin under {}/.ecp after admin index",
        repo_path.display()
    );
    found.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    found
        .pop()
        .expect("at least one graph.bin (asserted above)")
}

#[test]
fn a_mode_symbols_diff_two_graphs() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    // Initialize git repo
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git init failed");

    // Create baseline version (v1)
    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/functions.ts"), V1_CODE).expect("write v1");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git add v1");

    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@test.local",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git commit v1");

    // Index v1 and copy to baseline.bin
    let baseline_graph = index_repo(repo);
    let baseline_copy = repo.join("baseline.bin");
    eprintln!("baseline_graph from v1: {:?}", baseline_graph);
    std::fs::copy(&baseline_graph, &baseline_copy).expect("copy baseline.bin");
    let baseline_size = std::fs::metadata(&baseline_copy)
        .expect("baseline_copy metadata")
        .len();
    eprintln!("baseline.bin size: {}", baseline_size);

    // Update to v2
    std::fs::write(repo.join("src/functions.ts"), V2_CODE).expect("write v2");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git add v2");

    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@test.local",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "v2",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "git commit v2");

    // Index v2 and copy to current.bin
    let current_graph = index_repo(repo);
    let current_copy = repo.join("current.bin");
    eprintln!("current_graph from v2: {:?}", current_graph);
    std::fs::copy(&current_graph, &current_copy).expect("copy current.bin");
    let current_size = std::fs::metadata(&current_copy)
        .expect("current_copy metadata")
        .len();
    eprintln!("current.bin size: {}", current_size);

    // Run A-mode diff: --baseline-graph baseline.bin --current-graph current.bin
    let output = Command::new(ecp_bin())
        .args([
            "diff",
            "--section",
            "symbols",
            "--baseline-graph",
            baseline_copy.to_string_lossy().as_ref(),
            "--current-graph",
            current_copy.to_string_lossy().as_ref(),
            "--format",
            "json",
        ])
        .env("HOME", repo)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp diff A-mode");

    assert!(
        output.status.success(),
        "ecp diff A-mode failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("RAW STDOUT:\n{}", stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        eprintln!("PARSE ERROR: {e}");
        eprintln!("STDERR:\n{}", String::from_utf8_lossy(&output.stderr));
        panic!("invalid JSON: {e}; stdout: {stdout}")
    });

    // ── Validate top-level envelope ──────────────────────────────────────
    assert!(parsed.get("baseline").is_some(), "missing baseline");
    assert!(parsed.get("current").is_some(), "missing current");
    assert!(parsed.get("sections").is_some(), "missing sections");

    let baseline = &parsed["baseline"];
    let current = &parsed["current"];

    // In A-mode, baseline_sha and current_sha must be empty strings
    assert_eq!(
        baseline["sha"].as_str(),
        Some(""),
        "A-mode baseline_sha should be empty"
    );
    assert_eq!(
        current["sha"].as_str(),
        Some(""),
        "A-mode current_sha should be empty"
    );

    // baseline_ref and current_ref should reflect file paths (from mod.rs:138-141)
    assert!(baseline["ref"].as_str().is_some(), "baseline_ref missing");
    assert!(current["ref"].as_str().is_some(), "current_ref missing");

    // ── Validate symbols section ─────────────────────────────────────────
    let symbols = &parsed["sections"]["symbols"];
    assert!(symbols.get("certain").is_some(), "missing certain bucket");

    let certain = &symbols["certain"];

    // bar should be in symbols_added
    let added = certain["symbols_added"]
        .as_array()
        .expect("symbols_added must be array");
    let bar_added = added
        .iter()
        .any(|s| s.get("name").and_then(|n| n.as_str()) == Some("bar"));
    assert!(bar_added, "expected 'bar' in symbols_added; got: {added:?}");

    // foo should be in symbols_changed (body changed, content_hash differs)
    let changed = certain["symbols_changed"]
        .as_array()
        .expect("symbols_changed must be array");
    let foo_changed = changed
        .iter()
        .any(|s| s.get("name").and_then(|n| n.as_str()) == Some("foo"));
    assert!(
        foo_changed,
        "expected 'foo' in symbols_changed; got: {changed:?}"
    );
}

#[test]
fn a_mode_rejects_bindings_section() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    // Initialize git repo
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Create minimal file
    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/index.ts"), "export {};").expect("write");

    let out = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = Command::new("git")
        .args([
            "-c",
            "user.email=test@test.local",
            "-c",
            "user.name=Test",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Index and copy graphs
    let baseline_graph = index_repo(repo);
    let baseline_copy = repo.join("baseline.bin");
    std::fs::copy(&baseline_graph, &baseline_copy).expect("copy baseline.bin");

    let current_copy = repo.join("current.bin");
    std::fs::copy(&baseline_graph, &current_copy).expect("copy current.bin");

    // Attempt to run A-mode with bindings section (should fail)
    let output = Command::new(ecp_bin())
        .args([
            "diff",
            "--section",
            "bindings",
            "--baseline-graph",
            baseline_copy.to_string_lossy().as_ref(),
            "--current-graph",
            current_copy.to_string_lossy().as_ref(),
            "--format",
            "json",
        ])
        .env("HOME", repo)
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp diff");

    assert!(
        !output.status.success(),
        "ecp diff with --section bindings should have failed"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bindings cannot run in --baseline-graph mode"),
        "expected error about bindings in A-mode, got: {stderr}"
    );
}

#[test]
fn a_mode_requires_both_graph_flags() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    // Create a dummy graph file
    let dummy_graph = repo.join("dummy.bin");
    std::fs::write(&dummy_graph, b"dummy").expect("write dummy graph");

    // Attempt to run with --baseline-graph but no --current-graph (should fail)
    let output = Command::new(ecp_bin())
        .args([
            "diff",
            "--section",
            "symbols",
            "--baseline-graph",
            dummy_graph.to_string_lossy().as_ref(),
        ])
        .output()
        .expect("run ecp diff");

    assert!(
        !output.status.success(),
        "ecp diff without --current-graph should have failed"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("current-graph") || stderr.contains("required"),
        "expected clap error mentioning current-graph or required, got: {stderr}"
    );
}
