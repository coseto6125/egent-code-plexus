//! C4 `RecordKind::PathLiteral` in Swift (slice P6). After the parser
//! finishes, a path literal binds the innermost enclosing callable or `Const`
//! by span, else the file, and the `UsesPathLiteral` edge (what `ecp impact`
//! reads) starts there. Before P6 the pass looked the source up by name: the
//! first same-name node of the file, and no edge at module top level.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::RawEndpoint;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PATH: &str = "test.swift";

/// The incremental parse entry (`parse_file_raw`), which binds the records.
fn parse(src: &str) -> LocalGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(SwiftProvider::new().expect("SwiftProvider::new")));
    pipeline
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

/// `(name, 1-based line)` of the raw node the parse entry bound the literal
/// `value` to; `None` for the `File` endpoint.
fn bound_node(graph: &LocalGraph, value: &str) -> Option<(String, u32)> {
    let literal = graph
        .path_literals
        .iter()
        .flat_map(|literals| literals.iter())
        .find(|literal| literal.value == value)
        .unwrap_or_else(|| panic!("no path literal {value:?}"));
    match literal
        .endpoint
        .as_ref()
        .expect("the parse entry binds every literal")
    {
        RawEndpoint::Node(idx) => {
            let node = &graph.nodes[idx.index()];
            Some((node.name.clone(), node.span.0 + 1))
        }
        RawEndpoint::File => None,
        other => panic!("a path literal binds a node or the file, not {other:?}"),
    }
}

fn is_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Constructor
    )
}

const METHOD_SRC: &str = r#"class Store {
    func load() -> String {
        return read("conf/app.json")
    }
}
"#;

/// A literal in a method binds that method, and its edge starts there.
#[test]
fn test_bind_site_swift_path_literal_in_method_starts_at_method() {
    let graph = parse(METHOD_SRC);
    assert_eq!(
        bound_node(&graph, "conf/app.json"),
        Some(("load".to_string(), 2))
    );
    let sources = edge_sources(&build(graph), "conf/app.json");
    assert_eq!(names_lines(&sources), vec![("load", 2)]);
    assert!(is_callable(sources[0].0), "{sources:?}");
}

const TOP_SRC: &str = r#"print("conf/top.json")

func other() {}
"#;

/// A top-level call statement sits in no callable and no `Const`, so it binds `File`.
/// Before P6 the literal had no enclosing symbol and got no edge.
#[test]
fn test_bind_site_swift_module_top_literal_binds_file() {
    let graph = parse(TOP_SRC);
    assert_eq!(bound_node(&graph, "conf/top.json"), None);
    assert_eq!(
        edge_sources(&build(graph), "conf/top.json"),
        vec![(NodeKind::File, "test.swift".to_string(), 1)]
    );
}

const TWO_SRC: &str = r#"class A {
    func load() -> String {
        return read("conf/a.json")
    }
}
class B {
    func load() -> String {
        return read("conf/b.json")
    }
}
"#;

/// Two classes of one file each declare `load`.
/// Before P6 both edges started at the first `load`: the pass looked the
/// source up by name (`lookup_in_file`).
#[test]
fn test_bind_site_swift_two_same_name_methods_each_gets_own_edge() {
    let graph = parse(TWO_SRC);
    assert_eq!(
        bound_node(&graph, "conf/a.json"),
        Some(("load".to_string(), 2))
    );
    assert_eq!(
        bound_node(&graph, "conf/b.json"),
        Some(("load".to_string(), 7))
    );
    let g = build(graph);
    assert_eq!(
        names_lines(&edge_sources(&g, "conf/a.json")),
        vec![("load", 2)]
    );
    assert_eq!(
        names_lines(&edge_sources(&g, "conf/b.json")),
        vec![("load", 7)]
    );
}

const FRAMEWORK_SRC: &str =
    "func uikit() {}\n\nclass Screen: UIViewController {\n    override func viewDidLoad() {}\n}\n";

/// C4 `RecordKind::FrameworkRef` (slice P7): the parser detects the
/// framework from `UIViewController` anywhere in the file and writes a module-level
/// ref (`<module>`). The ref binds the file, so its `References` edge starts
/// at the `File` node. Before P7 the source was a name lookup of `<module>`,
/// which no node has, and the ref was dropped. The fixture declares a
/// callable named `uikit`, the only target the ref's name resolves to.
#[test]
fn test_framework_ref_module_level_binds_file() {
    let graph = build(parse(FRAMEWORK_SRC));
    let pool = graph.string_pool.as_slice();
    let edges: Vec<(NodeKind, String)> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::References && e.reason.resolve(pool) == "uikit-lifecycle"
        })
        .map(|e| {
            (
                graph.nodes[e.source as usize].kind,
                graph.nodes[e.target as usize]
                    .name
                    .resolve(pool)
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(edges, vec![(NodeKind::File, "uikit".to_string())]);
}
