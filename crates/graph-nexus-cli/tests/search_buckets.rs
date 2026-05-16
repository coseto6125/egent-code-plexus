use graph_nexus_core::graph::{
    File, FileCategory, Node, NodeKind, ZeroCopyGraph, GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
};
use graph_nexus_core::pool::StringPool;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

/// Build a graph with one function per category. Each function name contains
/// "widget" so all of them match the same search query.
fn make_bucket_graph() -> ZeroCopyGraph {
    let mut pool = StringPool::new();

    let src_path = pool.add("src/widget.rs");
    let test_path = pool.add("tests/widget_test.rs");
    let ref_path = pool.add("vendor/tree-sitter/src/widget_grammar.c");
    let doc_path = pool.add("docs/widget.md");
    let cfg_path = pool.add("config/widget.toml");

    let src_node_name = pool.add("widget_source");
    let test_node_name = pool.add("widget_test_fn");
    let ref_node_name = pool.add("widget_ref");
    let doc_node_name = pool.add("widget_doc");
    let cfg_node_name = pool.add("widget_cfg");

    let nodes = vec![
        Node {
            uid: pool.add("Function:src/widget.rs:widget_source"),
            name: src_node_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        },
        Node {
            uid: pool.add("Function:tests/widget_test.rs:widget_test_fn"),
            name: test_node_name,
            file_idx: 1,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        },
        Node {
            uid: pool.add("Function:vendor/tree-sitter/src/widget_grammar.c:widget_ref"),
            name: ref_node_name,
            file_idx: 2,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        },
        Node {
            uid: pool.add("Document:docs/widget.md:widget_doc"),
            name: doc_node_name,
            file_idx: 3,
            kind: NodeKind::Document,
            span: (1, 0, 5, 0),
            community_id: 0,
        },
        Node {
            uid: pool.add("Function:config/widget.toml:widget_cfg"),
            name: cfg_node_name,
            file_idx: 4,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        },
    ];

    let files = vec![
        File {
            path: src_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        },
        File {
            path: test_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Test,
        },
        File {
            path: ref_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Reference,
        },
        File {
            path: doc_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Document,
        },
        File {
            path: cfg_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Config,
        },
    ];

    let n = nodes.len() as u32;
    ZeroCopyGraph {
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
    }
}

fn write_graph(path: &Path, graph: &ZeroCopyGraph) {
    let bytes = rkyv::to_bytes::<Error>(graph).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes.as_slice()).unwrap();
}

fn setup_fixture() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    let graph = make_bucket_graph();
    write_graph(&graph_path, &graph);
    (tmp, graph_path)
}

fn run_search(graph: &Path, args: &[&str]) -> std::process::Output {
    Command::new(gnx_bin())
        .arg("search")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .output()
        .expect("gnx search spawn")
}

fn parse_json_output(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "no JSON in stdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    serde_json::from_str(&stdout[json_start..]).expect("valid JSON")
}

// ── Five-bucket keys present ──────────────────────────────────────────────────

#[test]
fn json_output_has_five_bucket_keys() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_output(&out);
    for key in &[
        "source",
        "tests",
        "reference",
        "document",
        "config",
        "status",
    ] {
        assert!(json.get(key).is_some(), "missing key '{key}' in: {json}");
    }
    assert!(
        json.get("results").is_none(),
        "old 'results' key must not appear"
    );
}

// ── Each bucket contains the correct hit ─────────────────────────────────────

#[test]
fn source_bucket_contains_source_hit() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "json"]);
    let json = parse_json_output(&out);
    let source = json["source"].as_array().expect("source is array");
    assert!(!source.is_empty(), "source bucket should be non-empty");
    assert!(
        source
            .iter()
            .any(|h| h["name"].as_str() == Some("widget_source")),
        "widget_source missing from source bucket: {source:?}"
    );
}

#[test]
fn tests_bucket_contains_test_hit() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "json"]);
    let json = parse_json_output(&out);
    let tests = json["tests"].as_array().expect("tests is array");
    assert!(!tests.is_empty(), "tests bucket should be non-empty");
    assert!(
        tests
            .iter()
            .any(|h| h["name"].as_str() == Some("widget_test_fn")),
        "widget_test_fn missing from tests bucket: {tests:?}"
    );
}

#[test]
fn reference_bucket_contains_vendor_hit() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "json"]);
    let json = parse_json_output(&out);
    let reference = json["reference"].as_array().expect("reference is array");
    assert!(
        !reference.is_empty(),
        "reference bucket should be non-empty"
    );
    assert!(
        reference
            .iter()
            .any(|h| h["name"].as_str() == Some("widget_ref")),
        "widget_ref missing from reference bucket: {reference:?}"
    );
}

// ── Language field is populated ───────────────────────────────────────────────

#[test]
fn language_field_populated_from_extension() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "json"]);
    let json = parse_json_output(&out);

    // src/widget.rs → Rust
    let source_hits = json["source"].as_array().unwrap();
    let src_hit = source_hits
        .iter()
        .find(|h| h["name"].as_str() == Some("widget_source"))
        .expect("widget_source hit");
    assert_eq!(
        src_hit["language"].as_str(),
        Some("Rust"),
        "src/widget.rs should have language=Rust, got: {src_hit}"
    );

    // vendor/tree-sitter/src/widget_grammar.c → C
    let ref_hits = json["reference"].as_array().unwrap();
    let ref_hit = ref_hits
        .iter()
        .find(|h| h["name"].as_str() == Some("widget_ref"))
        .expect("widget_ref hit");
    assert_eq!(
        ref_hit["language"].as_str(),
        Some("C"),
        "vendor .c file should have language=C, got: {ref_hit}"
    );
}

// ── Empty buckets emit [] not missing keys ────────────────────────────────────

#[test]
fn empty_buckets_emit_empty_array_in_json() {
    let mut pool = StringPool::new();
    let src_path = pool.add("src/only_source.rs");
    let uid_ref = pool.add("Function:src/only_source.rs:only_source_fn");
    let src_name = pool.add("only_source_fn");
    let n = 1u32;
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: src_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes: vec![Node {
            uid: uid_ref,
            name: src_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        }],
        edges: vec![],
        out_offsets: vec![0; 2],
        in_offsets: vec![0; 2],
        in_edge_idx: vec![],
        name_index: vec![0],
        process_start: n,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    write_graph(&graph_path, &graph);

    let out = run_search(&graph_path, &["only_source_fn", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_output(&out);

    // All 5 bucket keys must be present; empty ones are `[]`.
    for key in &["source", "tests", "reference", "document", "config"] {
        let bucket = json[key]
            .as_array()
            .unwrap_or_else(|| panic!("bucket '{key}' must be array, got: {}", json[key]));
        if *key == "source" {
            assert!(!bucket.is_empty(), "source bucket must be non-empty");
        } else {
            assert!(
                bucket.is_empty(),
                "bucket '{key}' should be empty [], got: {bucket:?}"
            );
        }
    }
}

// ── Text format uses === bucket === headers ───────────────────────────────────

#[test]
fn text_format_emits_section_headers() {
    let (_tmp, graph) = setup_fixture();
    let out = run_search(&graph, &["widget", "--format", "text"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for header in &[
        "=== source ===",
        "=== tests ===",
        "=== reference ===",
        "=== document ===",
        "=== config ===",
    ] {
        assert!(
            stdout.contains(header),
            "missing '{header}' in text output:\n{stdout}"
        );
    }
}

#[test]
fn text_format_empty_bucket_shows_none() {
    // Only source file — tests/reference/document/config buckets should show (none).
    let mut pool = StringPool::new();
    let src_path = pool.add("src/widget_only.rs");
    let uid_ref = pool.add("Function:src/widget_only.rs:widget_only");
    let src_name = pool.add("widget_only");
    let n = 1u32;
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: src_path,
            mtime: 0,
            content_hash: [0; 32],
            category: FileCategory::Source,
        }],
        nodes: vec![Node {
            uid: uid_ref,
            name: src_name,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (1, 0, 5, 0),
            community_id: 0,
        }],
        edges: vec![],
        out_offsets: vec![0; 2],
        in_offsets: vec![0; 2],
        in_edge_idx: vec![],
        name_index: vec![0],
        process_start: n,
        traces_offsets: vec![0],
        traces_data: vec![],
        blind_spots: vec![],
        route_shapes: vec![],
    };
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    write_graph(&graph_path, &graph);

    let out = run_search(&graph_path, &["widget_only", "--format", "text"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Tests / reference / document / config buckets are empty — expect "(none)".
    let none_count = stdout.matches("(none)").count();
    assert!(
        none_count >= 4,
        "expected at least 4 '(none)' lines for empty buckets, got {none_count} in:\n{stdout}"
    );
}

// ── TOP_K cap per bucket ──────────────────────────────────────────────────────

#[test]
fn each_bucket_independently_capped_at_top_k() {
    // Build a graph with 25 source functions all named "overflow_src_N".
    let mut pool = StringPool::new();
    let src_path = pool.add("src/big.rs");
    // Pre-allocate all StrRefs before building nodes vec.
    let node_data: Vec<(
        graph_nexus_core::pool::StrRef,
        graph_nexus_core::pool::StrRef,
    )> = (0..25usize)
        .map(|i| {
            let name = format!("overflow_src_{i}");
            let uid = format!("Function:src/big.rs:{name}");
            (pool.add(&uid), pool.add(&name))
        })
        .collect();
    let nodes: Vec<Node> = node_data
        .iter()
        .enumerate()
        .map(|(i, (uid_ref, name_ref))| Node {
            uid: *uid_ref,
            name: *name_ref,
            file_idx: 0,
            kind: NodeKind::Function,
            span: (i as u32, 0, i as u32 + 1, 0),
            community_id: 0,
        })
        .collect();
    let n = nodes.len() as u32;
    let graph = ZeroCopyGraph {
        magic: GRAPH_MAGIC,
        version: GRAPH_FORMAT_VERSION,
        fingerprint: [0; 32],
        string_pool: pool.bytes,
        files: vec![File {
            path: src_path,
            mtime: 0,
            content_hash: [0; 32],
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
    };
    let tmp = TempDir::new().unwrap();
    let graph_path = tmp.path().join("graph.bin");
    write_graph(&graph_path, &graph);

    let out = run_search(&graph_path, &["overflow_src", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_output(&out);
    let source_bucket = json["source"].as_array().expect("source is array");
    assert!(
        source_bucket.len() <= 20,
        "source bucket must be capped at TOP_K=20, got {} hits",
        source_bucket.len()
    );
}
