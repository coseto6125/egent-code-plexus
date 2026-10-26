//! Pins today's identity and call binding for a function-local named function
//! in C, before any lexical-scope opt-in slice changes it.
//!
//! Each assertion states what the builder produces today, including the
//! answers a later slice flips; the comment on such an assertion names the
//! slice. Expected values come from reading the parser and the builder; the
//! fixture was also indexed with `ecp` at 30530045 and its rows match.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PATH: &str = "test.c";
const SRC: &str = r#"#ifdef A
static int helper(void) { return 1; }
#else
static int helper(void) { return 2; }
#endif

int outer(void) {
    return helper();
}
"#;

fn parse() -> LocalGraph {
    CProvider::new()
        .expect("CProvider::new")
        .parse_file(Path::new(PATH), SRC.as_bytes())
        .expect("parse_file")
}

fn graph() -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse());
    builder.build()
}

/// One non-File node as the builder stores it; `line` is 1-based.
#[derive(Debug)]
struct Row {
    name: String,
    kind: NodeKind,
    line: u32,
    uid: u64,
}

fn rows(g: &ZeroCopyGraph) -> Vec<Row> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<Row> = g
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File)
        .map(|n| Row {
            name: n.name.resolve(pool).to_string(),
            kind: n.kind,
            line: n.span.0 + 1,
            uid: n.uid,
        })
        .collect();
    out.sort_by_key(|r| r.line);
    out
}

fn at(rows: &[Row], line: u32) -> &Row {
    rows.iter()
        .find(|r| r.line == line)
        .unwrap_or_else(|| panic!("no node at line {line}: {rows:?}"))
}

fn uid_of(kind: NodeKind, owner: Option<&str>, name: &str) -> u64 {
    ecp_core::uid::compute(kind, PATH, owner, name)
}

/// `(line, owner, kind)` of every `Calls` target of the node named `outer`.
fn outer_call_targets(g: &ZeroCopyGraph) -> Vec<(u32, String, NodeKind)> {
    let pool = g.string_pool.as_slice();
    g.edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == "outer")
        .map(|e| {
            let t = &g.nodes[e.target as usize];
            (
                t.span.0 + 1,
                t.owner_class.resolve(pool).to_string(),
                t.kind,
            )
        })
        .collect()
}

/// Raw parser nodes called `name`, in source order.
fn named<'a>(g: &'a LocalGraph, name: &str) -> Vec<&'a ecp_core::analyzer::types::RawNode> {
    let mut out: Vec<_> = g.nodes.iter().filter(|n| n.name == name).collect();
    out.sort_by_key(|n| n.span.0);
    out
}

// C has no nested functions. The closest collision is two same-named `static` definitions in
// opposite `#ifdef` branches: the second one is a tombstone. No lexical-scope slice opts C in.

#[test]
fn test_parse_file_c_nested_fn_same_name_as_top_level_today_uid() {
    let rows = rows(&graph());
    let first = at(&rows, 2);
    let second = at(&rows, 4);
    assert_eq!(
        (first.name.as_str(), first.kind),
        ("helper", NodeKind::Function)
    );
    assert_eq!(first.uid, uid_of(NodeKind::Function, None, "helper"));
    assert_eq!(
        (second.name.as_str(), second.kind),
        ("", NodeKind::Function)
    );
    assert_eq!(second.uid, ecp_core::uid::tombstone(first.uid));
}

#[test]
fn test_parse_file_c_nested_fn_owner_today() {
    let g = parse();
    let helpers = named(&g, "helper");
    assert_eq!(helpers.len(), 2);
    assert_eq!(helpers[0].owner_class, None);
    assert_eq!(helpers[1].owner_class, None);
}

#[test]
fn test_resolve_call_c_call_to_bare_helper_binds_today() {
    // The call binds the surviving first definition (line 2).
    assert_eq!(
        outer_call_targets(&graph()),
        vec![(2, String::new(), NodeKind::Function)]
    );
}
