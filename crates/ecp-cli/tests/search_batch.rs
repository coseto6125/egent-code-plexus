//! `ecp find --batch --mode bm25` reads patterns from stdin (one per
//! line, `#` comments and empty lines skipped) and emits a per-query
//! block prefixed by `=== pattern: <pattern> ===`. The point is to
//! amortise Engine load + mmap setup + tantivy open across N queries
//! inside a single process.

use ecp_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

struct BatchFixture {
    _home: TempDir,
    graph: PathBuf,
}

fn setup_fixture() -> BatchFixture {
    let home = TempDir::new().unwrap();
    let mut pool = StringPool::new();
    let file_path = pool.add("src.rs");
    let node_names = ["compute_hits", "build_hit"];
    let nodes: Vec<Node> = node_names
        .iter()
        .enumerate()
        .map(|(i, name)| Node {
            uid: ecp_core::uid::compute(NodeKind::Function, "src.rs", None, name),
            name: pool.add(name),
            file_idx: 0,
            kind: NodeKind::Function,
            span: (i as u32, 0, i as u32 + 1, 0),
            community_id: 0,
            owner_class: StrRef::default(),
        })
        .collect();
    let n = nodes.len() as u32;
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: file_path,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }],
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
        call_metas: vec![],
        function_metas: vec![],
    };
    let graph_path = home.path().join("graph.bin");
    std::fs::write(&graph_path, rkyv::to_bytes::<Error>(&graph).unwrap()).unwrap();
    BatchFixture {
        _home: home,
        graph: graph_path,
    }
}

fn run_batch_with_stdin(
    fixture: &BatchFixture,
    stdin_payload: &str,
    extra_args: &[&str],
) -> std::process::Output {
    let mut args = vec!["find", "--batch", "--mode", "bm25"];
    args.extend_from_slice(extra_args);
    let mut child = Command::new(ecp_bin())
        .args(args)
        .arg("--graph")
        .arg(&fixture.graph)
        .env("HOME", fixture._home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn batch_emits_per_query_divider_lines() {
    let fixture = setup_fixture();
    let payload = "compute_hits\nbuild_hit\n";
    let out = run_batch_with_stdin(&fixture, payload, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Each query's block must start with the divider.
    assert!(
        stdout.contains("=== pattern: compute_hits ==="),
        "missing divider for compute_hits in:\n{stdout}"
    );
    assert!(
        stdout.contains("=== pattern: build_hit ==="),
        "missing divider for build_hit in:\n{stdout}"
    );
}

#[test]
fn batch_skips_blank_and_commented_lines() {
    let fixture = setup_fixture();
    // 5 lines on stdin but only 1 is a real query.
    let payload = "\n# this is a comment\n   \ncompute_hits\n# trailing comment\n";
    let out = run_batch_with_stdin(&fixture, payload, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let divider_count = stdout.matches("=== pattern: ").count();
    assert_eq!(
        divider_count, 1,
        "expected exactly 1 divider (only 'compute_hits' is a real query), got {divider_count} in:\n{stdout}"
    );
    assert!(stdout.contains("=== pattern: compute_hits ==="));
}

#[test]
fn batch_with_empty_stdin_emits_no_query_dividers() {
    let fixture = setup_fixture();
    let payload = "\n# only comments\n\n";
    let out = run_batch_with_stdin(&fixture, payload, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("=== pattern: "),
        "expected no query dividers, got:\n{stdout}"
    );
    // The non-empty-input contract: emit a one-line stderr hint.
    assert!(
        stderr.contains("batch: no patterns on stdin"),
        "expected stderr hint, got:\n{stderr}"
    );
}
