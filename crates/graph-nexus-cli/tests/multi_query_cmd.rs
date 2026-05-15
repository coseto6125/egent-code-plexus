//! Integration tests for `gnx multi_query`.
//!
//! Drives the compiled binary end-to-end against a synthetic
//! `~/.gnx/` layout containing two registered repos with real graph
//! files, exercising:
//! - Repo-set resolution (`--repos a,b` / `--group g1` / `--all`)
//! - Concurrent per-repo graph loading via rayon
//! - Top-K BinaryHeap merge across repo boundaries
//! - Missing-graph degradation (a repo registered without `graph.bin`
//!   contributes to `repos_failed` but doesn't abort the call)
//! - `--repos` / `--group` / `--all` mutual exclusion + error message
//!   when none is supplied

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

/// Build a one-node graph whose single node carries `node_name`, then
/// serialize to `~/.gnx/<repo>/<branch>/graph.bin` and register the
/// repo in `registry.json` so `gnx multi_query` can find it.
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
    let graph_path = index_dir.join("graph.bin");
    std::fs::write(&graph_path, &bytes).unwrap();
    index_dir
}

fn write_registry(home_gnx: &Path, file: &RegistryFile) {
    std::fs::create_dir_all(home_gnx).unwrap();
    let path = home_gnx.join("registry.json");
    let json = serde_json::to_vec_pretty(file).unwrap();
    std::fs::write(&path, &json).unwrap();
}

fn run_multi_query(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(gnx_bin())
        .arg("multi_query")
        .args(args)
        .env("HOME", home)
        .output()
        .expect("gnx multi_query spawn")
}

struct Fixture {
    _home: TempDir,
    home_path: PathBuf,
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
                group: Some("g1".into()),
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
                group: Some("g1".into()),
            },
        ],
        groups: vec![GroupEntry {
            name: "g1".into(),
            members: vec!["alpha".into(), "beta".into()],
        }],
    };
    write_registry(&home_gnx, &registry);

    let home_path = home.path().to_path_buf();
    Fixture {
        _home: home,
        home_path,
    }
}

#[test]
fn multi_query_all_searches_both_repos() {
    let f = two_repo_fixture();
    let out = run_multi_query(
        &f.home_path,
        &["--all", "--query", "fetch", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: Value = serde_json::from_str(&stdout).expect(&stdout);
    let results = json["results"].as_array().cloned().unwrap_or_default();
    let repos: Vec<&str> = results.iter().filter_map(|r| r["repo"].as_str()).collect();
    assert!(repos.contains(&"alpha"), "alpha missing in {results:?}");
    assert!(repos.contains(&"beta"), "beta missing in {results:?}");
}

#[test]
fn multi_query_explicit_repos_csv() {
    let f = two_repo_fixture();
    let out = run_multi_query(
        &f.home_path,
        &["--repos", "alpha", "--query", "user", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = json["results"].as_array().unwrap();
    // Both alpha symbols match 'user' (fetch_user, save_user); beta excluded.
    assert!(!results.is_empty());
    for r in results {
        assert_eq!(r["repo"].as_str(), Some("alpha"));
    }
}

#[test]
fn multi_query_group_expands_via_registry() {
    let f = two_repo_fixture();
    let out = run_multi_query(
        &f.home_path,
        &["--group", "g1", "--query", "session", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = json["results"].as_array().unwrap();
    // Only beta has delete_session; alpha contributes 0 hits but the
    // group expansion still walks both repos.
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["repo"].as_str(), Some("beta"));
    assert_eq!(results[0]["name"].as_str(), Some("delete_session"));
}

#[test]
fn multi_query_top_k_bounds_merged_set() {
    let f = two_repo_fixture();
    let out = run_multi_query(
        &f.home_path,
        &["--all", "--query", "e", "--top-k", "2", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = json["results"].as_array().unwrap();
    // 'e' appears in all 4 names — top-K cap holds the merged Vec at 2.
    assert_eq!(results.len(), 2, "top_k=2 not honoured: {results:?}");
}

#[test]
fn multi_query_missing_graph_degrades_gracefully() {
    let f = two_repo_fixture();
    // Delete alpha's graph.bin to simulate a stale / un-analyzed repo.
    let alpha_graph = f.home_path.join(".gnx/alpha/main/graph.bin");
    std::fs::remove_file(&alpha_graph).unwrap();

    let out = run_multi_query(
        &f.home_path,
        &["--all", "--query", "fetch", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let summary = json["summary"].as_str().unwrap();
    // beta still produces fetch_account; alpha is the failed one.
    assert!(summary.contains("1 failed"), "summary: {summary}");
    let results = json["results"].as_array().unwrap();
    for r in results {
        assert_eq!(r["repo"].as_str(), Some("beta"));
    }
}

#[test]
fn multi_query_requires_one_of_repos_group_all() {
    let f = two_repo_fixture();
    let out = run_multi_query(&f.home_path, &["--query", "x", "--format", "json"]);
    assert!(!out.status.success(), "should fail with no selector");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--repos") && stderr.contains("--group") && stderr.contains("--all"),
        "stderr should hint at the 3 selectors: {stderr}"
    );
}
