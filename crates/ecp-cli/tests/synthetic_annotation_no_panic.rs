//! Regression test for `SYNTHETIC_FILE_IDX` (u32::MAX) sentinel handling.
//!
//! Synthetic `Annotation` nodes are emitted by `decorates_edges` post-process
//! when a `Decorates` edge's target cannot be resolved to a real declaration
//! (e.g. external / third-party decorators). They carry `file_idx == u32::MAX`
//! and indexing `graph.files[node.file_idx]` panics with index-out-of-bounds.
//!
//! Pre-fix, the following commands panicked when any synthetic Annotation was
//! reachable from query results:
//!   - `ecp find <name>` (name lookup catches synthetics by name)
//!   - `ecp inspect <name>` (search_nodes filter missing has_owning_file)
//!   - `ecp impact --baseline <ref>` (diff symbols + BFS reaches synthetics)
//!   - `ecp routes` (BFS upstream from handler over Decorates edges)
//!   - `ecp cypher MATCH (n) WHERE n.filePath = '...' RETURN n.filePath`
//!
//! Each command is now guarded via `Node::has_owning_file()` (added in PR #370).
//! This test sets up a repo with three external decorators that all resolve-miss,
//! commits + modifies + reindexes, and runs each command to assert no panic.

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

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ecp(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp failed to spawn")
}

/// Set up a Python repo with external decorators that trigger resolver-miss,
/// committed at HEAD~1 and amended at HEAD so `ecp impact --baseline HEAD~1`
/// has changed_symbols to walk.
fn setup_decorator_repo(repo: &Path) {
    // External decorators (`@app.route`, `@functools.lru_cache`, `@custom`)
    // are not defined anywhere in this repo → decorates_edges emits synthetic
    // Annotation nodes with SYNTHETIC_FILE_IDX.
    write(
        repo,
        "handlers.py",
        r#"
@app.route("/users")
def list_users():
    return []

@functools.lru_cache
def cached_lookup(key):
    return key

@custom
def decorated_helper():
    return 42
"#,
    );
    write(
        repo,
        "client.py",
        r#"
import requests

def call_users():
    return requests.get("/users")
"#,
    );

    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);

    // Modify handler body so impact --baseline HEAD~1 has a diff.
    write(
        repo,
        "handlers.py",
        r#"
@app.route("/users")
def list_users():
    return [{"id": 1}]

@functools.lru_cache
def cached_lookup(key):
    return key.upper()

@custom
def decorated_helper():
    return 42
"#,
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "modify"]);

    let out = ecp(repo, &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One repo + one `admin index` shared across all command probes. Each command
/// exits 0 (success) or 1 (graceful "not found" / "no diff") — anything else
/// (especially 101 = panic abort) means a synthetic node leaked through a guard.
#[test]
fn synthetic_annotation_does_not_panic_across_commands() {
    let tmp = tempfile::tempdir().unwrap();
    setup_decorator_repo(tmp.path());

    // Cover every CLI path that PR #372 touched. Names chosen to overlap with
    // synthetic Annotation labels (`route`, `lru_cache`) so the search hits
    // synthetics and the guard is the difference between success and panic.
    let probes: &[(&str, &[&str])] = &[
        (
            "find (name lookup)",
            &["find", "lru_cache", "--repo", ".", "--format", "json"],
        ),
        (
            "inspect (search_nodes filter)",
            &[
                "inspect", "--name", "route", "--format", "json", "--repo", ".",
            ],
        ),
        (
            "impact --baseline (diff iteration + BFS)",
            &[
                "impact",
                "--baseline",
                "HEAD~1",
                "--repo",
                ".",
                "--format",
                "json",
            ],
        ),
        (
            "cypher (filePath projection)",
            &[
                "cypher",
                "MATCH (n) RETURN n.filePath LIMIT 200",
                "--repo",
                ".",
                "--format",
                "json",
            ],
        ),
        (
            "rename --dry-run (target_indices builder)",
            &[
                "rename",
                "--symbol",
                "route",
                "--new-name",
                "endpoint",
                "--dry-run",
                "--repo",
                ".",
            ],
        ),
    ];

    for (label, args) in probes {
        let out = ecp(tmp.path(), args);
        let code = out.status.code().unwrap_or(-1);
        assert!(
            code == 0 || code == 1,
            "{label} crashed with code {code} (likely SYNTHETIC_FILE_IDX panic):\n  args={args:?}\n  stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
