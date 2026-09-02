//! `--graph <path>` names the graph to answer from. The session's L1 overlay
//! describes the *cwd repo's* uncommitted edits, so merging it into a foreign
//! graph answers a directed query with symbols that graph does not contain.
//!
//! The symptom that found this: under `cargo test` the child `ecp` inherits
//! the surrounding agent session's id, so a fixture-graph query answered with
//! paths from the real repository the test happened to run inside.

use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

const SESSION_ID: &str = "custom-graph-overlay-scope";

fn run_git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ecp` run from inside `repo`, with the cache root and the session id both
/// pinned to the fixture — the same shape an agent host produces.
fn run_ecp(repo: &Path, home: &Path, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .args(["--format", "json"])
        .current_dir(repo)
        .env("HOME", home)
        .env("ECP_SESSION_ID", SESSION_ID)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp spawn");
    assert!(
        out.status.success(),
        "ecp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
    serde_json::from_str(&stdout[start..]).expect("ecp emitted JSON")
}

/// A graph belonging to no repository on disk, holding one symbol nothing
/// else has.
fn write_foreign_graph(path: &Path) {
    let mut fx = GraphFixture::new();
    let id = fx.func("foreign/only.rs", "foreign_only_symbol");
    fx.span(id, (1, 0, 2, 0));
    std::fs::write(path, rkyv::to_bytes::<Error>(&fx.build()).unwrap()).unwrap();
}

#[test]
fn custom_graph_query_does_not_see_the_cwd_session_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("src/lib.rs"), "pub fn committed_symbol() {}\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-qm", "init"]);

    let index = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", repo.to_str().unwrap()])
        .env("HOME", &home)
        .output()
        .expect("ecp admin index spawn");
    assert!(
        index.status.success(),
        "admin index: {}",
        String::from_utf8_lossy(&index.stderr)
    );

    // Uncommitted edit: this symbol exists only in the L1 overlay.
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn committed_symbol() {}\npub fn overlay_only_symbol() {}\n",
    )
    .unwrap();

    // The overlay is live: cwd's own graph answers with the uncommitted symbol.
    let own = run_ecp(&repo, &home, &["find", "overlay_only_symbol"]);
    assert_eq!(
        own["found"], true,
        "fixture is not exercising an overlay: {own}"
    );

    let foreign = tmp.path().join("foreign-graph.bin");
    write_foreign_graph(&foreign);
    let graph_arg = foreign.to_str().unwrap();

    // The named graph answers, and it answers alone.
    let hit = run_ecp(
        &repo,
        &home,
        &["find", "foreign_only_symbol", "--graph", graph_arg],
    );
    assert_eq!(hit["found"], true, "the named graph must be the one read");
    assert_eq!(hit["matches"][0]["file"], "foreign/only.rs");

    let leaked = run_ecp(
        &repo,
        &home,
        &["find", "overlay_only_symbol", "--graph", graph_arg],
    );
    assert_eq!(
        leaked["found"], false,
        "cwd's overlay leaked into a --graph query: {leaked}"
    );
}
