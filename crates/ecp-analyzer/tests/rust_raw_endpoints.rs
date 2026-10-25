//! C4 `RecordKind::PathLiteral` in Rust (slice P6). After the parser
//! finishes, a path literal binds the innermost enclosing callable or `Const`
//! by span, else the file, and the `UsesPathLiteral` edge (what `ecp impact`
//! reads) starts there. Before P6 the pass looked the source up by name: the
//! first same-name node of the file, and no edge at module top level.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::RawEndpoint;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::{Path, PathBuf};

const PATH: &str = "test.rs";

fn pipeline() -> AnalyzerPipeline {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(RustProvider::new().expect("RustProvider::new")));
    pipeline
}

/// The incremental parse entry (`parse_file_raw`), which binds the records.
fn parse(src: &str) -> LocalGraph {
    pipeline()
        .parse_file_raw(Path::new(PATH), src.as_bytes())
        .expect("parse_file_raw")
}

fn build(graph: LocalGraph) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(graph);
    builder.build()
}

/// `(kind, name, 1-based line)` of the source of each `UsesPathLiteral` edge
/// into the path literal `value`, by line.
fn edge_sources(g: &ZeroCopyGraph, value: &str) -> Vec<(NodeKind, String, u32)> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::UsesPathLiteral)
        .filter(|e| {
            let target = &g.nodes[e.target as usize];
            target.kind == NodeKind::PathLiteral && target.name.resolve(pool) == value
        })
        .map(|e| {
            let source = &g.nodes[e.source as usize];
            (
                source.kind,
                source.name.resolve(pool).to_string(),
                source.span.0 + 1,
            )
        })
        .collect();
    out.sort_by(|a, b| (a.2, &a.1).cmp(&(b.2, &b.1)));
    out
}

/// `(name, line)` of each edge source.
fn names_lines(sources: &[(NodeKind, String, u32)]) -> Vec<(&str, u32)> {
    sources
        .iter()
        .map(|(_, name, line)| (name.as_str(), *line))
        .collect()
}

fn literal_endpoint(graph: &LocalGraph, value: &str) -> Option<RawEndpoint> {
    graph
        .path_literals
        .iter()
        .flat_map(|literals| literals.iter())
        .find(|literal| literal.value == value)
        .unwrap_or_else(|| panic!("no path literal {value:?}"))
        .endpoint
        .clone()
}

/// `(name, 1-based line)` of the raw node the parse entry bound the literal
/// `value` to; `None` for the `File` endpoint.
fn bound_node(graph: &LocalGraph, value: &str) -> Option<(String, u32)> {
    match literal_endpoint(graph, value).expect("the parse entry binds every literal") {
        RawEndpoint::Node(idx) => {
            let node = &graph.nodes[idx.index()];
            Some((node.name.clone(), node.span.0 + 1))
        }
        RawEndpoint::File => None,
        other => panic!("a path literal binds a node or the file, not {other:?}"),
    }
}

/// The bodiless trait declaration comes first: a name lookup of `load`
/// finds it before the impl method that holds the literal (C4 Stored data).
const METHOD_SRC: &str = r#"trait Loader {
    fn load(&self) -> String;
}

struct Disk;

impl Loader for Disk {
    fn load(&self) -> String {
        std::fs::read_to_string("conf/app.json").unwrap()
    }
}
"#;

/// A literal in a trait-impl method binds that method, and its edge starts
/// there, not at the trait's declaration of `load`.
#[test]
fn test_bind_site_rust_path_literal_in_trait_impl_method_starts_at_method() {
    let graph = parse(METHOD_SRC);
    assert_eq!(
        bound_node(&graph, "conf/app.json"),
        Some(("load".to_string(), 8))
    );
    let sources = edge_sources(&build(graph), "conf/app.json");
    assert_eq!(names_lines(&sources), vec![("load", 8)]);
    assert_eq!(sources[0].0, NodeKind::Method, "{sources:?}");
}

/// The owner of the `PathLiteral` node, and so its uid, is the bound node's
/// owner (`Disk`), as Pass 1 stores it.
#[test]
fn test_bind_site_rust_path_literal_uid_hashes_bound_method_owner() {
    let g = build(parse(METHOD_SRC));
    let pool = g.string_pool.as_slice();
    let literal = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::PathLiteral && n.name.resolve(pool) == "conf/app.json")
        .expect("PathLiteral node");
    assert_eq!(literal.owner_class.resolve(pool), "Disk");
    assert_eq!(
        literal.uid,
        ecp_core::uid::compute(NodeKind::PathLiteral, PATH, Some("Disk"), "conf/app.json")
    );
}

/// An attribute sits outside every item span, so its literal binds `File`.
/// Rust `const` and `static` items are `Const` nodes and bind themselves.
const TOP_SRC: &str = r#"#[doc = "conf/top.json"]
struct Top;
"#;

/// Before P6 the literal had no enclosing symbol and got no edge.
#[test]
fn test_bind_site_rust_module_top_literal_binds_file() {
    let graph = parse(TOP_SRC);
    assert_eq!(bound_node(&graph, "conf/top.json"), None);
    assert_eq!(
        edge_sources(&build(graph), "conf/top.json"),
        vec![(NodeKind::File, "test.rs".to_string(), 1)]
    );
}

const TWO_SRC: &str = r#"struct A;
struct B;

impl A {
    fn load(&self) -> String {
        std::fs::read_to_string("conf/a.json").unwrap()
    }
}

impl B {
    fn load(&self) -> String {
        std::fs::read_to_string("conf/b.json").unwrap()
    }
}
"#;

/// Two impls of one file each declare `load`; Rust rejects two free
/// functions of one name. Before P6 both edges started at the first `load`:
/// the pass looked the source up by name (`lookup_in_file`).
#[test]
fn test_bind_site_rust_two_same_name_methods_each_gets_own_edge() {
    let graph = parse(TWO_SRC);
    assert_eq!(
        bound_node(&graph, "conf/a.json"),
        Some(("load".to_string(), 5))
    );
    assert_eq!(
        bound_node(&graph, "conf/b.json"),
        Some(("load".to_string(), 11))
    );
    let g = build(graph);
    assert_eq!(
        names_lines(&edge_sources(&g, "conf/a.json")),
        vec![("load", 5)]
    );
    assert_eq!(
        names_lines(&edge_sources(&g, "conf/b.json")),
        vec![("load", 11)]
    );
}

/// The cold entry (`analyze`, the full index) binds as the incremental entry
/// does: both go through one bind step.
#[test]
fn test_bind_records_rust_cold_entry_binds_like_incremental_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abs = dir.path().join(PATH);
    std::fs::write(&abs, METHOD_SRC).expect("write fixture");
    let graphs = pipeline().analyze(vec![(abs, PathBuf::from(PATH))]);
    let [cold] = graphs.as_slice() else {
        panic!("one graph expected, got {}", graphs.len());
    };
    let incremental = parse(METHOD_SRC);
    let endpoint = literal_endpoint(cold, "conf/app.json");
    assert!(
        matches!(endpoint, Some(RawEndpoint::Node(_))),
        "{endpoint:?}"
    );
    assert_eq!(endpoint, literal_endpoint(&incremental, "conf/app.json"));
}

/// A provider called directly skips both parse entries, so its records carry
/// no endpoint. The pass binds them from the same site, and the edge still
/// starts at the method.
#[test]
fn test_endpoint_in_rust_direct_provider_call_binds_at_build() {
    let graph = RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new(PATH), METHOD_SRC.as_bytes())
        .expect("parse_file");
    assert_eq!(literal_endpoint(&graph, "conf/app.json"), None);
    assert_eq!(
        names_lines(&edge_sources(&build(graph), "conf/app.json")),
        vec![("load", 8)]
    );
}
