//! The read context roots every graph query at the worktree top
//! (architecture direction, area 5).
//!
//! `git status --porcelain` names paths relative to the worktree top. The
//! freshness gate joined them onto the directory the query ran in, so from
//! `pkg/sub` an uncommitted `pkg/sub/a.py` became `pkg/sub/pkg/sub/a.py`, was
//! never parsed, and `find` answered `found:false` for code on disk. The
//! engines that registry selectors, `group` and `peers plan` load never
//! attached the session overlay at all, so the same symbol was absent there
//! from any directory.

mod common;

use common::{commit_all, ecp_bin, run_git, write};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One id for every process of a test, the shape an agent host produces:
/// the overlay one query writes is the overlay the next one reads.
const SESSION_ID: &str = "read-context-test";

struct Fixture {
    _tmp: tempfile::TempDir,
    /// The worktree top; the name holds a space, so no step may split it.
    repo: PathBuf,
    home: PathBuf,
    /// A directory outside every repository.
    outside: PathBuf,
}

/// A committed, indexed repository with `pkg/sub/a.py`, then one uncommitted
/// function added to that file: `bar` exists only in the working tree.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("my repo");
    let home = tmp.path().join("home");
    let outside = tmp.path().join("outside");
    for dir in [&repo, &home, &outside] {
        std::fs::create_dir_all(dir).unwrap();
    }
    write(&repo, "pkg/sub/a.py", "def foo():\n    return 1\n");
    run_git(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo, "init");

    let index = ecp(&repo, &home, &["admin", "index", "--repo", "."]);
    assert!(
        index.status.success(),
        "admin index: {}",
        String::from_utf8_lossy(&index.stderr)
    );

    write(
        &repo,
        "pkg/sub/a.py",
        "def foo():\n    return 1\n\n\ndef bar():\n    return 2\n",
    );
    Fixture {
        _tmp: tmp,
        repo,
        home,
        outside,
    }
}

fn ecp(cwd: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        // `resolve_home_ecp` reads ECP_HOME first; an exported value would
        // send the fixture's cache into the developer's real one.
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    for key in [
        "GEMINI_CLI_SESSION_ID",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("ECP_SESSION_ID", SESSION_ID)
        .output()
        .expect("ecp spawn")
}

fn ecp_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let mut all = args.to_vec();
    all.extend(["--format", "json"]);
    let out = ecp(cwd, home, &all);
    assert!(
        out.status.success(),
        "ecp {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in the stdout of ecp {args:?}: {stdout}"));
    serde_json::from_str(&stdout[start..])
        .unwrap_or_else(|e| panic!("non-JSON output of ecp {args:?} ({e}): {stdout}"))
}

/// Failed before this change: `found:false`. The gate ran `git status` in
/// `pkg/sub`, joined `pkg/sub/a.py` onto `pkg/sub`, and the parse found no such
/// file, so no overlay fragment held `bar`. From the worktree top the same
/// query answered `found:true`.
#[test]
fn test_read_context_from_subdir_dirty_set_rooted_at_worktree_top() {
    let fx = fixture();
    let sub = fx.repo.join("pkg/sub");

    let v = ecp_json(&sub, &fx.home, &["find", "bar", "--repo", "."]);

    assert_eq!(
        v["found"], true,
        "an uncommitted symbol must be found from a subdirectory: {v}"
    );
    assert_eq!(
        v["matches"][0]["file"], "pkg/sub/a.py",
        "the path must stay relative to the worktree top: {v}"
    );
}

/// Failed before this change: the `source` bucket was empty. A registry
/// selector loads its graph through `load_ensured`, which never attached the
/// session overlay, and the bm25 ranker read the base graph and its tantivy
/// index only, where a symbol added after the build cannot be.
#[test]
fn test_load_ensured_bm25_find_sees_uncommitted_symbol() {
    let fx = fixture();
    write(
        &fx.repo,
        "pkg/fresh.py",
        "def fresh_bm25_marker():\n    return 3\n",
    );
    // The registry key `admin index` gave the repository. Querying from a
    // directory outside it leaves `load_ensured` as the only loader.
    let key = std::fs::read_dir(fx.home.join(".ecp"))
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.path().join("commits").is_dir())
        .expect("admin index registered the repo")
        .file_name()
        .into_string()
        .unwrap();

    let v = ecp_json(
        &fx.outside,
        &fx.home,
        &[
            "find",
            "fresh_bm25_marker",
            "--mode",
            "bm25",
            "--repo",
            key.as_str(),
        ],
    );

    let hits = v["source"].as_array().cloned().unwrap_or_default();
    assert!(
        hits.iter()
            .any(|h| h["name"] == "fresh_bm25_marker" && h["file"] == "pkg/fresh.py"),
        "a bm25 query through a registry selector must list the uncommitted \
         symbol: {v}"
    );
}

/// Failed before this change: the first assertion failed (the subdirectory
/// bug above), so the test never reached the foreign graph. The second half
/// guards the rule `main.rs` had and the read context now owns: the overlay
/// holds this worktree's edits, so a `--graph` that names another graph reads
/// none.
#[test]
fn test_read_context_foreign_graph_keeps_no_overlay() {
    let fx = fixture();
    let sub = fx.repo.join("pkg/sub");

    let own = ecp_json(&sub, &fx.home, &["find", "bar"]);
    assert_eq!(
        own["found"], true,
        "the overlay must be live from the subdirectory: {own}"
    );

    let foreign = fx.home.join("foreign graph.bin");
    let mut graph = GraphFixture::new();
    let id = graph.func("foreign/only.rs", "foreign_only_symbol");
    graph.span(id, (1, 0, 2, 0));
    std::fs::write(&foreign, rkyv::to_bytes::<Error>(&graph.build()).unwrap()).unwrap();
    let graph_arg = foreign.to_str().unwrap();

    let named = ecp_json(
        &sub,
        &fx.home,
        &["find", "foreign_only_symbol", "--graph", graph_arg],
    );
    assert_eq!(
        named["found"], true,
        "the named graph must be read: {named}"
    );

    let leaked = ecp_json(&sub, &fx.home, &["find", "bar", "--graph", graph_arg]);
    assert_eq!(
        leaked["found"], false,
        "this worktree's overlay leaked into a foreign graph: {leaked}"
    );
}

/// Two-letter suffix for the `i`-th generated name; letters only, so no name
/// shares the query's `v2` tokens.
fn letters(i: usize) -> String {
    [b'a' + (i / 26) as u8, b'a' + (i % 26) as u8]
        .iter()
        .map(|&b| b as char)
        .collect()
}

/// Base rows carry tantivy BM25 scores, overlay rows substring scores (1.0
/// exact), and the per-bucket cut keeps the `TOP_K` (20) best. Failed before
/// this change: 25 base `parse_config_*` rows outscored the exact-name overlay
/// symbol and the cut dropped it from `source`.
#[test]
fn test_bm25_find_exact_overlay_symbol_ranks_ahead_of_base_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let matching: String = (0..25)
        .map(|i| format!("def parse_config_{}():\n    return 1\n\n\n", letters(i)))
        .collect();
    // Unrelated symbols give `parse` and `config` an IDF that lifts the
    // matching rows' BM25 scores above the substring scale.
    let filler: String = (0..200)
        .map(|i| format!("def filler_{}():\n    return 1\n\n\n", letters(i)))
        .collect();
    write(&repo, "pkg/parsers.py", &matching);
    write(&repo, "pkg/filler.py", &filler);
    run_git(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo, "init");
    let index = ecp(&repo, &home, &["admin", "index", "--repo", "."]);
    assert!(
        index.status.success(),
        "admin index: {}",
        String::from_utf8_lossy(&index.stderr)
    );
    write(
        &repo,
        "pkg/parsers_v2.py",
        "def parse_config_v2():\n    return 2\n",
    );

    let v = ecp_json(&repo, &home, &["find", "parse_config_v2", "--mode", "bm25"]);

    let source = v["source"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        source.len(),
        20,
        "setup: the source bucket must be cut: {v}"
    );
    let others: Vec<&Value> = source
        .iter()
        .filter(|h| h["name"] != "parse_config_v2")
        .collect();
    assert!(
        others
            .iter()
            .all(|h| h["score_source"] == "bm25" && h["score"].as_f64().unwrap_or(0.0) > 1.0),
        "setup: every base row must outscore 1.0, or the test cannot tell the \
         ranking apart from the score order: {v}"
    );
    let overlay = source
        .iter()
        .find(|h| h["name"] == "parse_config_v2")
        .unwrap_or_else(|| panic!("the exact-name overlay symbol was cut: {v}"));
    assert_eq!(overlay["file"], "pkg/parsers_v2.py", "{v}");
    assert_eq!(
        overlay["score_source"], "substring",
        "the score must stay the substring score it is: {v}"
    );
    assert_eq!(overlay["score"].as_f64(), Some(1.0), "{v}");
}

/// A new overlay symbol under `examples/` takes the category its path gives,
/// as a base row does. Failed before this change: the overlay knew only test
/// and source paths, so the bm25 row landed in `source` and the exact match
/// read `Source`.
#[test]
fn test_overlay_symbol_under_examples_categorised_example() {
    let fx = fixture();
    write(
        &fx.repo,
        "examples/demo.py",
        "def demo_example_marker():\n    return 4\n",
    );

    let bm25 = ecp_json(
        &fx.repo,
        &fx.home,
        &["find", "demo_example_marker", "--mode", "bm25"],
    );
    let examples = bm25["examples"].as_array().cloned().unwrap_or_default();
    assert!(
        examples
            .iter()
            .any(|h| h["name"] == "demo_example_marker" && h["file"] == "examples/demo.py"),
        "the bm25 row must land in `examples`: {bm25}"
    );

    let exact = ecp_json(&fx.repo, &fx.home, &["find", "demo_example_marker"]);
    assert_eq!(exact["found"], true, "{exact}");
    assert_eq!(exact["matches"][0]["category"], "Example", "{exact}");
}
