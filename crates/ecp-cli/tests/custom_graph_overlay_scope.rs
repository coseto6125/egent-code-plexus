//! `--graph <path>` names the graph to answer from. The session's L1 overlay
//! describes the *cwd repo's* uncommitted edits, so merging it into a foreign
//! graph answers a directed query with symbols that graph does not contain.
//! Pointing `--graph` at the file cwd would have resolved anyway names the
//! same graph, and must not change the answer.
//!
//! The symptom that found this: under `cargo test` the child `ecp` inherits
//! the surrounding agent session's id, so a fixture-graph query answered with
//! paths from the real repository the test happened to run inside.

mod common;

use common::{ecp_bin, run_git};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const SESSION_ID: &str = "custom-graph-overlay-scope";

/// An indexed repo with one uncommitted symbol, and the cache root holding
/// its graph. Both tests need the whole shape, so they share the setup.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

fn fixture() -> Fixture {
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
        .env_remove("ECP_HOME")
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

    Fixture {
        _tmp: tmp,
        repo,
        home,
    }
}

/// `ecp` run from inside the repo, with the cache root and the session id both
/// pinned to the fixture — the same shape an agent host produces.
fn run_ecp(fx: &Fixture, args: &[&str]) -> Value {
    let out = Command::new(ecp_bin())
        .args(args)
        .args(["--format", "json"])
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        // `resolve_home_ecp` reads ECP_HOME first; a developer who exports it
        // would have the fixture write its repo dir and registry entry into
        // the real cache root, and the assertions would still pass.
        .env_remove("ECP_HOME")
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

/// The published graph for the fixture's HEAD — what `--graph` would name if
/// an agent read the path out of the cache and passed it back.
fn published_graph(fx: &Fixture) -> PathBuf {
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&fx.repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    WalkDir::new(fx.home.join(".ecp"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.path().to_path_buf())
        .find(|p| {
            p.file_name().is_some_and(|n| n == "graph.bin")
                && p.parent()
                    .and_then(|d| d.file_name())
                    .is_some_and(|n| n.to_string_lossy().contains(&head))
        })
        .expect("a published graph.bin for HEAD")
}

#[test]
fn custom_graph_query_does_not_see_the_cwd_session_overlay() {
    let fx = fixture();

    // The overlay is live: cwd's own graph answers with the uncommitted symbol.
    let own = run_ecp(&fx, &["find", "overlay_only_symbol"]);
    assert_eq!(
        own["found"], true,
        "fixture is not exercising an overlay: {own}"
    );

    let foreign = fx.home.join("foreign-graph.bin");
    write_foreign_graph(&foreign);
    let graph_arg = foreign.to_str().unwrap();

    // The named graph answers, and it answers alone.
    let hit = run_ecp(&fx, &["find", "foreign_only_symbol", "--graph", graph_arg]);
    assert_eq!(hit["found"], true, "the named graph must be the one read");
    assert_eq!(hit["matches"][0]["file"], "foreign/only.rs");

    let leaked = run_ecp(&fx, &["find", "overlay_only_symbol", "--graph", graph_arg]);
    assert_eq!(
        leaked["found"], false,
        "cwd's overlay leaked into a --graph query: {leaked}"
    );
}

#[test]
fn naming_cwds_own_graph_explicitly_keeps_the_overlay() {
    let fx = fixture();
    // Populate the overlay first; the explicit path must then read the same
    // graph the bare invocation did, overlay included.
    assert_eq!(
        run_ecp(&fx, &["find", "overlay_only_symbol"])["found"],
        true
    );

    let own_path = published_graph(&fx);
    let explicit = run_ecp(
        &fx,
        &[
            "find",
            "overlay_only_symbol",
            "--graph",
            own_path.to_str().unwrap(),
        ],
    );
    assert_eq!(
        explicit["found"], true,
        "the same graph gave two different answers depending on how it was \
         named: {explicit}"
    );
}
