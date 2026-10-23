use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file")
}

fn graph(source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(source));
    builder.build()
}

fn owner_of(g: &LocalGraph, name: &str) -> Option<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| n.owner_class.clone())
}

#[test]
fn same_method_name_on_different_structs_is_distinguished() {
    let src = r#"
struct Foo;
struct Bar;
impl Foo {
    fn run(&self) {}
}
impl Bar {
    fn run(&self) {}
}
"#;
    let g = parse(src);
    // Both nodes named "run" exist — owner_class differentiates them.
    let runs: Vec<_> = g.nodes.iter().filter(|n| n.name == "run").collect();
    assert_eq!(
        runs.len(),
        2,
        "both run() methods must be emitted: {runs:?}"
    );
    let owners: Vec<_> = runs
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"Foo"),
        "one run must own Foo; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"Bar"),
        "one run must own Bar; owners: {owners:?}"
    );
}

#[test]
fn module_level_function_has_no_owner() {
    let src = r#"
struct Unused;
fn free_fn() {}
"#;
    let g = parse(src);
    let oc = owner_of(&g, "free_fn");
    assert!(
        oc.is_none(),
        "free_fn is module-level, owner_class must be None; got {oc:?}"
    );
}

#[test]
fn impl_method_owner_matches_struct() {
    let src = r#"
struct Calc;
impl Calc {
    fn add(&self) -> i32 { 0 }
}
"#;
    let g = parse(src);
    let oc = owner_of(&g, "add");
    assert_eq!(oc.as_deref(), Some("Calc"), "add must own Calc; got {oc:?}");
}

const RUST_NESTED_HELPER_UIDS: &str = r#"
struct A;
struct B;
fn marker_a() {}
fn marker_b() {}

impl A {
    fn new() -> Self {
        fn helper() { marker_a(); }
        helper();
        A
    }
}

impl B {
    fn new() -> Self {
        fn helper() { marker_b(); }
        helper();
        B
    }
}
"#;

#[test]
fn test_nested_rust_functions_under_same_named_methods_keep_distinct_uids_and_calls() {
    let local = parse(RUST_NESTED_HELPER_UIDS);
    let helper_hashes: Vec<_> = local
        .nodes
        .iter()
        .filter(|node| node.name == "helper")
        .map(|node| node.content_hash)
        .collect();
    assert_eq!(helper_hashes.len(), 2, "{local:#?}");
    assert_ne!(helper_hashes[0], helper_hashes[1], "{local:#?}");

    let graph = graph(RUST_NESTED_HELPER_UIDS);
    let pool = graph.string_pool.as_slice();
    let helpers: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.name.resolve(pool) == "helper")
        .collect();
    assert_eq!(helpers.len(), 2, "{graph:#?}");
    assert_ne!(helpers[0].1.uid, helpers[1].1.uid, "{graph:#?}");

    let b_new = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "new" && node.owner_class.resolve(pool) == "B")
        .map(|(idx, _)| idx as u32)
        .expect("B::new node");
    let b_helper = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.content_hash == helper_hashes[1])
        .map(|(idx, _)| idx as u32)
        .expect("B::new::helper node");
    let b_targets: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == b_new)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .map(|edge| edge.target)
        .collect();
    assert_eq!(b_targets, vec![b_helper], "{graph:#?}");
}

/// C6: lexical selection is a pre-existing resolver limit.  It remains
/// ignored until the resolver has caller-span ancestry, separate from C5 UID
/// preservation above.
#[test]
#[ignore = "C6: prefer_free has no lexical caller ancestry"]
fn test_c6_rust_top_level_and_nested_same_name_prefer_nested_function() {
    let source = r#"
fn helper() {}

fn nested_marker() {}

fn outer() {
    fn helper() { nested_marker(); }
    helper();
}
"#;
    let local = parse(source);
    let nested_hash = local
        .nodes
        .iter()
        .filter(|node| node.name == "helper")
        .nth(1)
        .map(|node| node.content_hash)
        .expect("nested helper raw node");
    let graph = graph(source);
    let pool = graph.string_pool.as_slice();
    let outer = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "outer")
        .map(|(idx, _)| idx as u32)
        .expect("outer node");
    let nested = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.content_hash == nested_hash)
        .map(|(idx, _)| idx as u32)
        .expect("nested helper node");
    let targets: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == outer)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .map(|edge| edge.target)
        .collect();
    assert_eq!(targets, vec![nested], "{graph:#?}");
}
