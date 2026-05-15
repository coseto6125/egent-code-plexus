//! PreToolUse hook: pattern extraction + in-process graph augmentation.
//! Covers no-op branches and the with-index → emit-hits branch (which
//! was deferred in PR #17 and is now reachable thanks to the
//! `TantivyEngine` wireup + 1-hop expansion in `compute_hits`).

use std::io::Write;
use std::process::{Command, Stdio};

use graph_nexus_cli::search::TantivyEngine;
use graph_nexus_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use std::fs;
use tempfile::tempdir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(envelope: &str) -> std::process::Output {
    let mut child = Command::new(gnx_bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn short_pattern_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"ab"}}"#);
    assert!(out.stdout.is_empty(), "<3 char pattern should no-op");
}

#[test]
fn missing_graph_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Grep","tool_input":{"pattern":"validateUser"}}"#);
    assert!(out.stdout.is_empty(), "no .gitnexus-rs/ in /tmp → no-op");
}

#[test]
fn bash_grep_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"rg -n 'validateUser' src/"}}"#,
    );
    assert!(
        out.stdout.is_empty(),
        "no index → no-op even with valid pattern"
    );
    assert!(out.status.success(), "hook must never fail on no-op");
}

#[test]
fn non_search_tool_no_op() {
    let out = run(r#"{"cwd":"/tmp","tool_name":"Read","tool_input":{"file_path":"foo"}}"#);
    assert!(out.stdout.is_empty());
}

#[test]
fn glob_pattern_with_no_index_no_op() {
    let out = run(
        r#"{"cwd":"/tmp","tool_name":"Glob","tool_input":{"pattern":"src/**/validateUser.rs"}}"#,
    );
    assert!(out.stdout.is_empty());
}

/// Build a minimal 3-node graph with one CALLS edge so the hook has
/// enough fixture to surface a hit + a `Called by:` line.
fn make_graph() -> ZeroCopyGraph {
    let mut pool = StringPool::new();
    let file_path = pool.add("src/lib.rs");
    let reason = pool.add("call");
    let load_uid = pool.add("Function:src/lib.rs:loadConfig");
    let load_name = pool.add("loadConfig");
    let parse_uid = pool.add("Function:src/lib.rs:parseConfig");
    let parse_name = pool.add("parseConfig");
    let tok_uid = pool.add("Function:src/lib.rs:tokenize");
    let tok_name = pool.add("tokenize");
    let mk = |uid, name, line: u32| Node {
        uid,
        name,
        file_idx: 0,
        kind: NodeKind::Function,
        span: (line, 0, line + 1, 0),
        community_id: 0,
    };
    // node 0 = parseConfig, 1 = loadConfig, 2 = tokenize.
    // edges: parseConfig→tokenize (e0), loadConfig→parseConfig (e1).
    let edges = vec![
        Edge {
            source: 0,
            target: 2,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason,
        },
        Edge {
            source: 1,
            target: 0,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason,
        },
    ];
    // out_offsets: parseConfig has 1 (e0), loadConfig has 1 (e1), tokenize 0.
    let out_offsets = vec![0u32, 1, 2, 2];
    // in_edge_idx + in_offsets: parseConfig has e1 in; tokenize has e0 in.
    let in_edge_idx = vec![1u32, 0];
    let in_offsets = vec![0u32, 1, 1, 2];

    ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes: vec![
            mk(parse_uid, parse_name, 10),
            mk(load_uid, load_name, 20),
            mk(tok_uid, tok_name, 30),
        ],
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index: vec![],
        embeddings: None,
        process_start: 3,
        traces_offsets: vec![],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    }
}

#[test]
fn with_index_emits_legacy_block_via_subprocess() {
    let tmp = tempdir().unwrap();
    let repo = tmp.path();
    let graph = make_graph();
    let gnx_dir = repo.join(".gitnexus-rs");
    fs::create_dir_all(&gnx_dir).unwrap();
    fs::write(
        gnx_dir.join("graph.bin"),
        rkyv::to_bytes::<Error>(&graph).unwrap().as_slice(),
    )
    .unwrap();
    TantivyEngine::build_index(repo, &graph).expect("tantivy build");

    let envelope = format!(
        r#"{{"cwd":"{}","tool_name":"Grep","tool_input":{{"pattern":"parseConfig"}}}}"#,
        repo.display()
    );
    let out = run(&envelope);
    assert!(
        out.status.success(),
        "hook must not error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("parseConfig"),
        "stdout should mention the matched symbol; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Called by: loadConfig"),
        "stdout should expose 1-hop callers; got:\n{stdout}"
    );
    assert!(
        stdout.contains("Calls: tokenize"),
        "stdout should expose 1-hop callees; got:\n{stdout}"
    );
}
