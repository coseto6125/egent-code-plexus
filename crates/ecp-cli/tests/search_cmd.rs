//! Integration tests for `ecp find --mode bm25` (formerly `ecp search`).
//!
//! Exercises:
//! - Single-repo BM25 search (positional pattern, replaces old `--query` flag)
//! - `--mode bm25` accepted; `vector` / `hybrid` / `auto` rejected by clap
//! - Multi-repo fan-out via `--repo @<group>` (port from multi_query_cmd)
//! - Missing-graph degradation in multi-repo mode
//! - Empty result returns "No matches" hint
//! - `--top-k` and `--query` flags are gone (regression guards)

use ecp_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::StringPool;
use ecp_core::registry::{GroupEntry, RegistryFile, RepoAlias};
use rkyv::rancor::Error;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ── Fixture helpers ─────────────────────────────────────────────────────────

/// Seed a graph under the v2 layout: `<home_ecp>/<dir_name>/commits/<sha>/graph.bin`.
/// Returns the path to the graph.bin.
fn seed_repo(home_ecp: &Path, dir_name: &str, sha_dir: &str, node_names: &[&str]) -> PathBuf {
    let mut pool = StringPool::new();
    let nodes: Vec<Node> = node_names
        .iter()
        .map(|n| Node {
            uid: pool.add(&format!("Function:{dir_name}.rs:{n}")),
            name: pool.add(n),
            file_idx: 0,
            kind: NodeKind::Function,
            span: (0, 0, 0, 10),
            community_id: 0,
        })
        .collect();
    let files = vec![File {
        path: pool.add(&format!("{dir_name}.rs")),
        mtime: 0,
        content_hash: [0; 8],
        category: FileCategory::Source,
    }];
    let n = nodes.len() as u32;
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files,
        nodes,
        edges: vec![],
        out_offsets: vec![0; (n + 1) as usize],
        in_offsets: vec![0; (n + 1) as usize],
        in_edge_idx: vec![],
        name_index: (0..n).collect(),
        process_start: n,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    let commit_dir = home_ecp.join(dir_name).join("commits").join(sha_dir);
    std::fs::create_dir_all(&commit_dir).unwrap();
    let graph_path = commit_dir.join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    graph_path
}

fn write_registry(home_ecp: &Path, file: &RegistryFile) {
    std::fs::create_dir_all(home_ecp).unwrap();
    let json = serde_json::to_vec_pretty(file).unwrap();
    std::fs::write(home_ecp.join("registry.json"), &json).unwrap();
}

struct Fixture {
    _home: TempDir,
    home_path: PathBuf,
    /// Path to alpha's graph.bin (used as --graph for single-repo tests).
    alpha_graph: PathBuf,
}

fn two_repo_fixture() -> Fixture {
    let home = TempDir::new().unwrap();
    let home_ecp = home.path().join(".ecp");

    let alpha_graph = seed_repo(
        &home_ecp,
        "alpha__aabbccdd",
        "sha_alpha0001",
        &["fetch_user", "save_user"],
    );
    let _beta_graph = seed_repo(
        &home_ecp,
        "beta__aabbccdd",
        "sha_beta00001",
        &["fetch_account", "delete_session"],
    );

    let mut repos = BTreeMap::new();
    repos.insert(
        "alpha__aabbccdd".into(),
        RepoAlias {
            dir_name: "alpha__aabbccdd".into(),
            common_dir: "/tmp/alpha/.git".into(),
            remote_url: Some("git@example:alpha".into()),
            aliases: vec!["alpha".into()],
            last_touched: "2026-05-16T00:00:00Z".into(),
            groups: vec!["g1".into()],
        },
    );
    repos.insert(
        "beta__aabbccdd".into(),
        RepoAlias {
            dir_name: "beta__aabbccdd".into(),
            common_dir: "/tmp/beta/.git".into(),
            remote_url: Some("git@example:beta".into()),
            aliases: vec!["beta".into()],
            last_touched: "2026-05-16T00:00:00Z".into(),
            groups: vec!["g1".into()],
        },
    );
    let registry = RegistryFile {
        version: 2,
        repos,
        groups: vec![GroupEntry {
            name: "g1".into(),
            members: vec!["alpha__aabbccdd".into(), "beta__aabbccdd".into()],
        }],
    };
    write_registry(&home_ecp, &registry);

    let home_path = home.path().to_path_buf();
    Fixture {
        _home: home,
        home_path,
        alpha_graph,
    }
}

// ── Helper: run `ecp find --mode bm25` pointing at a specific graph ──────────

fn run_search(home: &Path, graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("find")
        .arg("--mode")
        .arg("bm25")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .env("HOME", home)
        .output()
        .expect("ecp find spawn")
}

fn run_search_multi(home: &Path, args: &[&str]) -> std::process::Output {
    let alpha_graph = home.join(".ecp/alpha__aabbccdd/commits/sha_alpha0001/graph.bin");
    Command::new(ecp_bin())
        .arg("find")
        .arg("--mode")
        .arg("bm25")
        .args(args)
        .arg("--graph")
        .arg(&alpha_graph)
        .env("HOME", home)
        .output()
        .expect("ecp find spawn")
}

// ── Single-repo tests ─────────────────────────────────────────────────────────

#[test]
fn search_positional_pattern_finds_match() {
    let f = two_repo_fixture();
    let out = run_search(&f.home_path, &f.alpha_graph, &["fetch", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in: {stdout}"));
    let json: Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    // Output is now bucketed: check all 5 bucket keys are present.
    for key in &["source", "tests", "reference", "document", "config"] {
        assert!(json[key].is_array(), "expected bucket '{key}' in: {json}");
    }
    // At least one bucket must be non-empty for 'fetch'.
    let total: usize = ["source", "tests", "reference", "document", "config"]
        .iter()
        .map(|k| json[k].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    assert!(total > 0, "expected hits for 'fetch': {json}");
}

/// Direct-invocation helper for tests that exercise the `--mode` flag
/// surface itself (the standard `run_search` helper pins `--mode bm25`
/// to keep BM25-shape assertions stable, which would conflict with
/// explicit `--mode` values supplied by the test body).
fn run_find_raw(home: &Path, graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ecp_bin())
        .arg("find")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .env("HOME", home)
        .output()
        .expect("ecp find spawn")
}

#[test]
fn find_accepts_mode_bm25() {
    let f = two_repo_fixture();
    let out = run_find_raw(
        &f.home_path,
        &f.alpha_graph,
        &["fetch_user", "--mode", "bm25", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("error: "));
}

/// clap must reject any `--mode` value outside the supported enum
/// (`exact` / `fuzzy` / `bm25`) with `invalid value …`. The legacy
/// `vector` / `hybrid` / `auto` placeholders are gone for good.
#[test]
fn find_rejects_removed_modes() {
    let f = two_repo_fixture();
    for mode in &["vector", "hybrid", "auto"] {
        let out = run_find_raw(
            &f.home_path,
            &f.alpha_graph,
            &["fetch_user", "--mode", mode, "--format", "json"],
        );
        assert!(
            !out.status.success(),
            "--mode {mode} should be rejected by clap"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("invalid value"),
            "expected 'invalid value' in stderr for --mode {mode}, got: {stderr}"
        );
    }
}

#[test]
fn search_empty_result_includes_hint() {
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["zzzz_nonexistent_xyz", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No matches") || stdout.contains("hint"),
        "expected 'No matches' hint, got: {stdout}"
    );
}

#[test]
fn search_rejects_top_k_flag() {
    // --top-k was removed from the public surface.
    let f = two_repo_fixture();
    let out = run_search(&f.home_path, &f.alpha_graph, &["fetch", "--top-k", "5"]);
    assert!(
        !out.status.success(),
        "should reject --top-k; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn search_rejects_query_flag() {
    // Old --query flag is gone; positional pattern is required instead.
    let f = two_repo_fixture();
    let out = run_search(&f.home_path, &f.alpha_graph, &["--query", "fetch_user"]);
    assert!(
        !out.status.success(),
        "should reject --query; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ── Multi-repo tests ──────────────────────────────────────────────────────────

#[test]
fn search_multi_repo_at_all() {
    let f = two_repo_fixture();
    let out = run_search_multi(
        &f.home_path,
        &["fetch", "--repo", "@all", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("fetch"),
        "expected 'fetch' in results: {stdout}"
    );
}

#[test]
fn search_multi_repo_missing_graph_degrades_gracefully() {
    // Remove beta's graph to simulate a stale repo.
    // Alpha's graph is used by --graph (for main.rs engine load) and for
    // @all fan-out. Beta will fail silently; alpha still produces results.
    let f = two_repo_fixture();
    let beta_graph = f
        .home_path
        .join(".ecp/beta__aabbccdd/commits/sha_beta00001/graph.bin");
    std::fs::remove_file(&beta_graph).unwrap();

    let out = run_search_multi(
        &f.home_path,
        &["fetch", "--repo", "@all", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON: {stdout}"));
    let json: Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    // Alpha still has fetch_user — expect ≥1 result across buckets.
    let total: usize = ["source", "tests", "reference", "document", "config"]
        .iter()
        .map(|k| json[k].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    assert!(total > 0, "alpha should still produce hits: {json}");
}
