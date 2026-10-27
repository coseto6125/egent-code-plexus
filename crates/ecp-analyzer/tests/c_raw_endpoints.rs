//! C4 `RecordKind::PathLiteral` in C (slice P6). After the parser
//! finishes, a path literal binds the innermost enclosing callable or `Const`
//! by span, else the file, and the `UsesPathLiteral` edge (what `ecp impact`
//! reads) starts there. Before P6 the pass looked the source up by name: the
//! first same-name node of the file, and no edge at module top level.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::raw_records::RawEndpoint;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PATH: &str = "test.c";

/// The incremental parse entry (`parse_file_raw`), which binds the records.
fn parse(src: &str) -> LocalGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(CProvider::new().expect("CProvider::new")));
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

/// C has no methods: the callable is a function.
const FUNCTION_SRC: &str = r#"int load(void) {
    fopen("conf/app.json", "r");
    return 0;
}
"#;

/// A literal in a function binds that function, and its edge starts there.
#[test]
fn test_bind_site_c_path_literal_in_function_starts_at_function() {
    let graph = parse(FUNCTION_SRC);
    assert_eq!(
        bound_node(&graph, "conf/app.json"),
        Some(("load".to_string(), 1))
    );
    let sources = edge_sources(&build(graph), "conf/app.json");
    assert_eq!(names_lines(&sources), vec![("load", 1)]);
    assert!(is_callable(sources[0].0), "{sources:?}");
}

const TOP_SRC: &str = r#"static const char *top = "conf/top.json";

int other(void) { return 0; }
"#;

/// A global's initializer sits in no callable and no `Const` (the C parser
/// emits none), so it binds `File`.
/// Before P6 the literal had no enclosing symbol and got no edge.
#[test]
fn test_bind_site_c_module_top_literal_binds_file() {
    let graph = parse(TOP_SRC);
    assert_eq!(bound_node(&graph, "conf/top.json"), None);
    assert_eq!(
        edge_sources(&build(graph), "conf/top.json"),
        vec![(NodeKind::File, "test.c".to_string(), 1)]
    );
}

/// C rejects two definitions of one function and has no classes, so the
/// only same-name pair in one file is a pair of `#ifdef` twins. Pass 1 keeps
/// the first and tombstones the second (`c_lexical_scope.rs`).
const TWIN_SRC: &str = r#"#ifdef A
int load(void) {
    fopen("conf/a.json", "r");
    return 0;
}
#else
int load(void) {
    fopen("conf/b.json", "r");
    return 1;
}
#endif
"#;

/// Each literal binds its own twin by span. The second twin is a tombstone,
/// so its literal's edge starts at the survivor (C4 Open point 2, option a):
/// an edge never starts at a tombstone, whose name is empty.
#[test]
fn test_bind_site_c_ifdef_twin_functions_edges_start_at_survivor() {
    let graph = parse(TWIN_SRC);
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
        vec![("load", 2)]
    );
}
