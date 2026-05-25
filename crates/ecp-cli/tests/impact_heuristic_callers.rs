//! Characterization tests: pins the POST-CHANGE behavior where heuristic callers
//! are shown BY DEFAULT in `ecp impact` as a `heuristic_callers` JSON array,
//! each entry tagged `requires_verification: true`. `--no-heuristic` suppresses
//! the bucket and restores the old hidden-count-only behavior.
//!
//! The test injects a synthetic graph with an `EventTopicMirror` edge from
//! `publish_order` (publisher) to `consume_order` (subscriber) at confidence
//! 0.85, plus an unrelated plain function `unrelated_plain_fn` that has no
//! heuristic edge.

use ecp_core::graph::{
    Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph, GRAPH_FORMAT_VERSION,
    GRAPH_MAGIC,
};
use ecp_core::pool::{StrRef, StringPool};
use rkyv::rancor::Error;
use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// Redis publish/subscribe on "orders" — T5-33 emits EventTopicMirror at 0.85.
const PUBLISHER_SRC: &str = r#"
import redis

def publish_order(r, data):
    r.publish("orders", data)
"#;

const SUBSCRIBER_SRC: &str = r#"
import redis

def consume_order(pubsub):
    pubsub.subscribe("orders")
"#;

const PLAIN_SRC: &str = r#"
def unrelated_plain_fn():
    pass
"#;

/// Initialise a git repo, write the two Python fixtures, and run `admin index`.
fn init_repo_with_fixtures(repo: &Path) {
    std::fs::create_dir_all(repo.join("svc")).unwrap();
    std::fs::write(repo.join("svc/publisher.py"), PUBLISHER_SRC).unwrap();
    std::fs::write(repo.join("svc/subscriber.py"), SUBSCRIBER_SRC).unwrap();
    std::fs::write(repo.join("svc/plain.py"), PLAIN_SRC).unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["add", "-A"]);
    run_git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Locate the `graph.bin` produced under `.ecp/`.
fn find_graph_bin(repo: &Path) -> std::path::PathBuf {
    fn walk(dir: &Path, depth: usize) -> Option<std::path::PathBuf> {
        if depth == 0 {
            return None;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return None;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.file_name().map(|n| n == "graph.bin").unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = walk(&p, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(&repo.join(".ecp"), 5).expect("graph.bin not found after admin index")
}

/// Synthetic three-node graph:
///   `publish_order`     (idx 0, svc/publisher.py)
///     ──[EventTopicMirror, 0.85]──▶
///   `consume_order`     (idx 1, svc/subscriber.py)
///   `unrelated_plain_fn` (idx 2, svc/plain.py)  — no heuristic edges
///
/// Upstream BFS from `consume_order` will reach the heuristic edge.
/// Upstream BFS from `unrelated_plain_fn` will find no heuristic edges.
fn synthetic_event_mirror_graph() -> Vec<u8> {
    let mut pool = StringPool::new();
    let file_pub = pool.add("svc/publisher.py");
    let file_sub = pool.add("svc/subscriber.py");
    let file_plain = pool.add("svc/plain.py");

    let pub_uid = ecp_core::uid::compute(
        NodeKind::Function,
        "svc/publisher.py",
        None,
        "publish_order",
    );
    let sub_uid = ecp_core::uid::compute(
        NodeKind::Function,
        "svc/subscriber.py",
        None,
        "consume_order",
    );
    let plain_uid = ecp_core::uid::compute(
        NodeKind::Function,
        "svc/plain.py",
        None,
        "unrelated_plain_fn",
    );

    let pub_name = pool.add("publish_order");
    let sub_name = pool.add("consume_order");
    let plain_name = pool.add("unrelated_plain_fn");
    let reason_ref = pool.add("redis-pubsub-orders");

    let files = vec![
        File {
            path: file_pub,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: file_sub,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
        File {
            path: file_plain,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        },
    ];

    let nodes = vec![
        Node {
            uid: pub_uid,
            name: pub_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (4, 0, 5, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        },
        Node {
            uid: sub_uid,
            name: sub_name,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (4, 0, 5, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        },
        Node {
            uid: plain_uid,
            name: plain_name,
            file_idx: 2,
            kind: NodeKind::Function,
            span: (2, 0, 3, 0),
            community_id: 0,
            owner_class: StrRef::default(),
            content_hash: 0,
        },
    ];

    // publish_order (0) ──[EventTopicMirror, 0.85]──▶ consume_order (1)
    // unrelated_plain_fn (2) — no edges
    let edges = vec![Edge {
        source: 0,
        target: 1,
        rel_type: RelType::EventTopicMirror,
        confidence: 0.85,
        reason: reason_ref,
    }];

    // CSR outgoing: publish_order has edge 0; consume_order has none; plain has none.
    let out_offsets = vec![0u32, 1, 1, 1];
    // CSR incoming: publish_order has none; consume_order has edge 0; plain has none.
    let in_offsets = vec![0u32, 0, 1, 1];
    let in_edge_idx = vec![0u32];
    let name_index: Vec<ecp_core::graph::NameIndexEntry> = Vec::new();

    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files,
        nodes,
        edges,
        out_offsets,
        in_offsets,
        in_edge_idx,
        name_index,
        process_start: 2,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
        call_metas: vec![],
        function_metas: vec![],
        kind_offsets: vec![],
        kind_node_idx: vec![],
        node_flags: vec![],
    };

    rkyv::to_bytes::<Error>(&graph)
        .expect("serialize synthetic graph")
        .into_vec()
}

/// Invoke `ecp impact <symbol> --format json --repo .` (default, no --no-heuristic)
/// and return the parsed JSON payload.
fn run_impact_default(repo: &Path, symbol: &str) -> serde_json::Value {
    let out = Command::new(ecp_bin())
        .args(["impact", symbol, "--format", "json", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp impact failed to spawn");
    assert!(
        out.status.success(),
        "ecp impact exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout:\n{stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"))
}

/// Invoke `ecp impact <symbol> --no-heuristic --format json --repo .`
/// and return the parsed JSON payload.
fn run_impact_no_heuristic(repo: &Path, symbol: &str) -> serde_json::Value {
    let out = Command::new(ecp_bin())
        .args([
            "impact",
            symbol,
            "--no-heuristic",
            "--format",
            "json",
            "--repo",
            ".",
        ])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("ecp impact --no-heuristic failed to spawn");
    assert!(
        out.status.success(),
        "ecp impact --no-heuristic exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("no JSON in stdout:\n{stdout}"));
    serde_json::from_str(&stdout[json_start..])
        .unwrap_or_else(|e| panic!("JSON parse failed: {e}\nstdout={stdout}"))
}

/// Default `impact` (no flag) MUST include heuristic callers as a non-empty
/// array, with each entry tagged `requires_verification: true`.
///
/// Target: `consume_order` (subscriber) — upstream BFS reaches the
/// EventTopicMirror edge from `publish_order`.
#[test]
fn impact_default_shows_heuristic_callers_tagged() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    init_repo_with_fixtures(repo);
    let graph_bin = find_graph_bin(repo);
    std::fs::write(&graph_bin, synthetic_event_mirror_graph()).unwrap();

    let payload = run_impact_default(repo, "consume_order");

    let heuristic_callers = payload
        .get("heuristic_callers")
        .unwrap_or_else(|| panic!("heuristic_callers must be present by default; got: {payload}"));
    let arr = heuristic_callers
        .as_array()
        .unwrap_or_else(|| panic!("heuristic_callers must be an array; got: {heuristic_callers}"));
    assert!(
        !arr.is_empty(),
        "heuristic_callers must be non-empty for consume_order; got: {payload}"
    );
    assert_eq!(
        arr[0]["requires_verification"],
        serde_json::Value::Bool(true),
        "each heuristic caller must be tagged requires_verification=true; got: {payload}"
    );
}

/// `--no-heuristic` MUST suppress the `heuristic_callers` bucket entirely
/// (key absent) and report a non-zero `hidden_heuristic_edges` count.
///
/// Target: `consume_order` — same symbol as test 1, flag inverts behavior.
#[test]
fn impact_no_heuristic_suppresses_bucket() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    init_repo_with_fixtures(repo);
    let graph_bin = find_graph_bin(repo);
    std::fs::write(&graph_bin, synthetic_event_mirror_graph()).unwrap();

    let payload = run_impact_no_heuristic(repo, "consume_order");

    assert!(
        payload.get("heuristic_callers").is_none(),
        "--no-heuristic: heuristic_callers must be absent; got: {payload}"
    );
    assert!(
        payload["hidden_heuristic_edges"].as_u64().unwrap_or(0) >= 1,
        "--no-heuristic: the EventTopicMirror edge must be counted as hidden; got: {payload}"
    );
}

/// A symbol with NO incoming heuristic edge (`unrelated_plain_fn`) must still
/// emit `heuristic_callers: []` (key present, empty array) under the default
/// behavior — so consumers can always branch on the key existing.
#[test]
fn impact_deterministic_only_symbol_has_empty_heuristic_bucket() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    init_repo_with_fixtures(repo);
    let graph_bin = find_graph_bin(repo);
    std::fs::write(&graph_bin, synthetic_event_mirror_graph()).unwrap();

    let payload = run_impact_default(repo, "unrelated_plain_fn");

    let heuristic_callers = payload.get("heuristic_callers").unwrap_or_else(|| {
        panic!("heuristic_callers must be present even when empty; got: {payload}")
    });
    assert_eq!(
        heuristic_callers,
        &serde_json::Value::Array(vec![]),
        "heuristic_callers must be an empty array for a symbol with no heuristic edges; got: {payload}"
    );
}
