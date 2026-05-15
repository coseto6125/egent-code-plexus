//! Integration tests for `gnx search`.
//!
//! Exercises:
//! - Single-repo BM25 search (positional pattern, replaces old `--query` flag)
//! - `--mode` flag accepted with all 4 values
//! - Auto-mode slug detection routes to BM25 (no fallback hint)
//! - Auto-mode phrase falls back to BM25 with a stderr hint (no embeddings)
//! - Multi-repo fan-out via `--repo @<group>` (port from multi_query_cmd)
//! - Missing-graph degradation in multi-repo mode
//! - Empty result returns "No matches" hint
//! - `--top-k` and `--query` flags are gone (regression guards)

use graph_nexus_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use graph_nexus_core::registry::{BranchEntry, GroupEntry, RegistryFile, RepoEntry};
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

// ── Fixture helpers (ported from multi_query_cmd.rs) ─────────────────────────

fn seed_repo(home_gnx: &Path, repo: &str, branch: &str, node_names: &[&str]) -> PathBuf {
    let mut pool = StringPool::new();
    let nodes: Vec<Node> = node_names
        .iter()
        .map(|n| Node {
            uid: pool.add(&format!("Function:{repo}.rs:{n}")),
            name: pool.add(n),
            file_idx: 0,
            kind: NodeKind::Function,
            span: (0, 0, 0, 10),
            community_id: 0,
        })
        .collect();
    let files = vec![File {
        path: pool.add(&format!("{repo}.rs")),
        mtime: 0,
        content_hash: [0; 32],
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
        embeddings: None,
        process_start: n,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    let bytes = rkyv::to_bytes::<Error>(&graph).unwrap();
    let index_dir = home_gnx.join(repo).join(branch);
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::write(index_dir.join("graph.bin"), &bytes).unwrap();
    index_dir
}

fn write_registry(home_gnx: &Path, file: &RegistryFile) {
    std::fs::create_dir_all(home_gnx).unwrap();
    let json = serde_json::to_vec_pretty(file).unwrap();
    std::fs::write(home_gnx.join("registry.json"), &json).unwrap();
}

struct Fixture {
    _home: TempDir,
    home_path: PathBuf,
    /// Path to alpha's graph.bin (used as --graph for single-repo tests).
    alpha_graph: PathBuf,
}

fn two_repo_fixture() -> Fixture {
    let home = TempDir::new().unwrap();
    let home_gnx = home.path().join(".gnx");

    let repo_a = seed_repo(&home_gnx, "alpha", "main", &["fetch_user", "save_user"]);
    let repo_b = seed_repo(
        &home_gnx,
        "beta",
        "main",
        &["fetch_account", "delete_session"],
    );

    let registry = RegistryFile {
        version: 1,
        repos: vec![
            RepoEntry {
                name: "alpha".into(),
                remote_url: "git@example:alpha".into(),
                worktree_path: "/tmp/alpha".into(),
                index_dir_root: home_gnx.to_string_lossy().into(),
                branches: vec![BranchEntry {
                    name: "main".into(),
                    index_dir: repo_a.to_string_lossy().into(),
                    indexed_at: "now".into(),
                    node_count: 2,
                    delta_size: 0,
                    embedding_status: "none".into(),
                }],
                groups: vec!["g1".into()],
            },
            RepoEntry {
                name: "beta".into(),
                remote_url: "git@example:beta".into(),
                worktree_path: "/tmp/beta".into(),
                index_dir_root: home_gnx.to_string_lossy().into(),
                branches: vec![BranchEntry {
                    name: "main".into(),
                    index_dir: repo_b.to_string_lossy().into(),
                    indexed_at: "now".into(),
                    node_count: 2,
                    delta_size: 0,
                    embedding_status: "none".into(),
                }],
                groups: vec!["g1".into()],
            },
        ],
        groups: vec![GroupEntry {
            name: "g1".into(),
            members: vec!["alpha".into(), "beta".into()],
        }],
    };
    write_registry(&home_gnx, &registry);

    let alpha_graph = home_gnx.join("alpha/main/graph.bin");
    let home_path = home.path().to_path_buf();
    Fixture {
        _home: home,
        home_path,
        alpha_graph,
    }
}

// ── Helper: run `gnx search` pointing at a specific graph ────────────────────

fn run_search(home: &Path, graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(gnx_bin())
        .arg("search")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .env("HOME", home)
        .output()
        .expect("gnx search spawn")
}

fn run_search_multi(home: &Path, args: &[&str]) -> std::process::Output {
    let alpha_graph = home.join(".gnx/alpha/main/graph.bin");
    Command::new(gnx_bin())
        .arg("search")
        .args(args)
        .arg("--graph")
        .arg(&alpha_graph)
        .env("HOME", home)
        .output()
        .expect("gnx search spawn")
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
    let results = json["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !results.is_empty(),
        "expected hits for 'fetch': {results:?}"
    );
}

#[test]
fn search_accepts_mode_bm25() {
    let f = two_repo_fixture();
    let out = run_search(
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

#[test]
fn search_accepts_mode_vector_stub() {
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["fetch_user", "--mode", "vector", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("error: "));
}

#[test]
fn search_accepts_mode_hybrid_stub() {
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["fetch_user", "--mode", "hybrid", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("error: "));
}

#[test]
fn search_accepts_mode_auto() {
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["fetch_user", "--mode", "auto", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn search_slug_input_auto_no_fallback_hint() {
    // Slug-like → bm25; should NOT emit the "falling back to bm25" hint.
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["fetch_user", "--mode", "auto", "--format", "json"],
    );
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("falling back to bm25"),
        "unexpected fallback hint for slug input: {stderr}"
    );
}

#[test]
fn search_phrase_input_auto_emits_fallback_hint() {
    // Phrase input + no embeddings → bm25 fallback with stderr hint.
    let f = two_repo_fixture();
    let out = run_search(
        &f.home_path,
        &f.alpha_graph,
        &["how does auth work", "--mode", "auto", "--format", "json"],
    );
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("falling back to bm25"),
        "expected fallback hint for phrase input, got: {stderr}"
    );
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
fn search_multi_repo_at_group_both_repos() {
    let f = two_repo_fixture();
    let out = run_search_multi(
        &f.home_path,
        &["fetch", "--repo", "@g1", "--format", "json"],
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
    let results = json["results"].as_array().cloned().unwrap_or_default();
    let repos: Vec<&str> = results.iter().filter_map(|r| r["repo"].as_str()).collect();
    assert!(repos.contains(&"alpha"), "alpha missing: {repos:?}");
    assert!(repos.contains(&"beta"), "beta missing: {repos:?}");
}

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
fn search_multi_repo_csv_single() {
    let f = two_repo_fixture();
    let out = run_search_multi(
        &f.home_path,
        &["user", "--repo", "alpha", "--format", "json"],
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
    let results = json["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !results.is_empty(),
        "alpha has fetch_user/save_user: {results:?}"
    );
    for r in &results {
        assert_eq!(r["repo"].as_str(), Some("alpha"), "unexpected repo: {r}");
    }
}

#[test]
fn search_multi_repo_unknown_group_errors() {
    let f = two_repo_fixture();
    let out = run_search_multi(
        &f.home_path,
        &["foo", "--repo", "@nonexistent_group", "--format", "json"],
    );
    assert!(!out.status.success(), "should fail for unknown group");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nonexistent_group") || stderr.contains("unknown group"),
        "stderr should mention the unknown group: {stderr}"
    );
}

#[test]
fn search_multi_repo_missing_graph_degrades_gracefully() {
    // Remove beta's graph to simulate a stale repo.
    // Alpha's graph is used by --graph (for main.rs engine load) and for
    // @all fan-out. Beta will fail silently; alpha still produces results.
    let f = two_repo_fixture();
    let beta_graph = f.home_path.join(".gnx/beta/main/graph.bin");
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
    // Alpha still has fetch_user — expect ≥1 result.
    let results = json["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !results.is_empty(),
        "alpha should still produce hits: {results:?}"
    );
}
