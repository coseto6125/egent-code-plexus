use ecp_core::graph::{FileCategory, NodeKind, ZeroCopyGraph};
use ecp_core::graph_fixture::GraphFixture;
use rkyv::rancor::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Build a graph with one function per category. Each function name contains
/// "widget" so all of them match the same search query.
fn make_bucket_graph() -> ZeroCopyGraph {
    let mut fx = GraphFixture::new();

    fx.file_as("src/widget.rs", FileCategory::Source);
    let src = fx.func("src/widget.rs", "widget_source");
    fx.span(src, (1, 0, 5, 0));

    fx.file_as("tests/widget_test.rs", FileCategory::Test);
    let test = fx.func("tests/widget_test.rs", "widget_test_fn");
    fx.span(test, (1, 0, 5, 0));

    fx.file_as(
        "vendor/tree-sitter/src/widget_grammar.c",
        FileCategory::Reference,
    );
    let reference = fx.node(
        NodeKind::Function,
        "vendor/tree-sitter/src/widget_grammar.c",
        "widget_ref",
    );
    fx.span(reference, (1, 0, 5, 0));

    fx.file_as("docs/widget.md", FileCategory::Document);
    let doc = fx.node(NodeKind::Document, "docs/widget.md", "widget_doc");
    fx.span(doc, (1, 0, 5, 0));

    fx.file_as("config/widget.toml", FileCategory::Config);
    let cfg = fx.func("config/widget.toml", "widget_cfg");
    fx.span(cfg, (1, 0, 5, 0));

    fx.build()
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
    Command::new(ecp_bin())
        .arg("find")
        .arg("--mode")
        .arg("bm25")
        .args(args)
        .arg("--graph")
        .arg(graph)
        .output()
        .expect("ecp find spawn")
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
    let mut fx = GraphFixture::new();
    let src = fx.func("src/only_source.rs", "only_source_fn");
    fx.span(src, (1, 0, 5, 0));
    let graph = fx.build();
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
    let mut fx = GraphFixture::new();
    let src = fx.func("src/widget_only.rs", "widget_only");
    fx.span(src, (1, 0, 5, 0));
    let graph = fx.build();
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
    let mut fx = GraphFixture::new();
    for i in 0..25usize {
        let name = format!("overflow_src_{i}");
        let id = fx.func("src/big.rs", &name);
        fx.span(id, (i as u32, 0, i as u32 + 1, 0));
    }
    let graph = fx.build();
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
