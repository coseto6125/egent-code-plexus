use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    parse_at("test.rs", source)
}

fn parse_at(path: &str, source: &str) -> LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new(path), source.as_bytes())
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
    let helper_owners: Vec<_> = local
        .nodes
        .iter()
        .filter(|node| node.name == "helper")
        .filter_map(|node| node.owner_class.as_deref())
        .collect();
    assert!(helper_owners.contains(&"A::new"), "{local:#?}");
    assert!(helper_owners.contains(&"B::new"), "{local:#?}");

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

#[test]
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

#[test]
fn test_nested_rust_function_without_local_declaration_does_not_bind_sibling_scope() {
    let source = r#"
struct A;
struct B;
fn marker_a() {}

impl A {
    fn new() -> Self {
        fn helper() { marker_a(); }
        A
    }
}

impl B {
    fn new() -> Self {
        helper();
        B
    }
}
"#;
    let graph = graph(source);
    let pool = graph.string_pool.as_slice();
    let b_new = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "new" && node.owner_class.resolve(pool) == "B")
        .map(|(idx, _)| idx as u32)
        .expect("B::new node");
    let helper_targets: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == b_new)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .collect();
    assert!(helper_targets.is_empty(), "{graph:#?}");
}

#[test]
fn test_nested_rust_function_in_another_file_does_not_bind_global_fallback() {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse_at(
        "a.rs",
        r#"
struct A;
impl A {
    fn new() -> Self {
        fn helper() {}
        A
    }
}
"#,
    ));
    builder.add_graph(parse_at(
        "b.rs",
        r#"
fn caller() {
    helper();
}
"#,
    ));
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let caller = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "caller")
        .map(|(idx, _)| idx as u32)
        .expect("caller node");
    let helper_targets: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == caller)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .collect();
    assert!(helper_targets.is_empty(), "{graph:#?}");
}

#[test]
fn test_rust_module_free_function_remains_callable_from_module_member() {
    let source = r#"
mod services {
    fn helper() {}
    fn caller() {
        helper();
    }
}
"#;
    let graph = graph(source);
    let pool = graph.string_pool.as_slice();
    let caller = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "caller")
        .map(|(idx, _)| idx as u32)
        .expect("caller node");
    let helper = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "helper")
        .map(|(idx, _)| idx as u32)
        .expect("helper node");
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.rel_type == RelType::Calls
                && edge.source == caller
                && edge.target == helper),
        "{graph:#?}"
    );
}

#[test]
fn test_rust_trait_impl_nested_functions_keep_distinct_uids() {
    let source = r#"
struct A;

impl std::fmt::Display for A {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn pad() { let _ = 1; }
        pad();
        Ok(())
    }
}

impl std::fmt::Debug for A {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn pad() { let _ = 2; }
        pad();
        Ok(())
    }
}
"#;
    let graph = graph(source);
    let pool = graph.string_pool.as_slice();
    let pads: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| node.name.resolve(pool) == "pad")
        .collect();
    assert_eq!(pads.len(), 2, "{graph:#?}");
    assert_ne!(pads[0].uid, pads[1].uid, "{graph:#?}");
    for (caller_id, caller) in graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.name.resolve(pool) == "fmt")
    {
        let expected: Vec<_> = graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.name.resolve(pool) == "pad"
                    && ecp_core::analyzer::types::span_contains(caller.span, node.span)
            })
            .map(|(id, _)| id as u32)
            .collect();
        let actual: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.rel_type == RelType::Calls && edge.source == caller_id as u32)
            .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "pad")
            .map(|edge| edge.target)
            .collect();
        assert_eq!(expected.len(), 1);
        assert_eq!(actual, expected);
    }
}

#[test]
fn test_resolve_call_rust_local_impl_binds_own_nested_helper() {
    let graph = graph("fn helper() {}\nfn outer() { struct S; impl S { fn run() { fn helper() { let _ = 1; } helper(); } } }");
    let pool = &graph.string_pool;
    let (caller_id, caller) = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "run")
        .unwrap();
    let expected: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.name.resolve(pool) == "helper"
                && ecp_core::analyzer::types::span_contains(caller.span, node.span)
        })
        .map(|(id, _)| id as u32)
        .collect();
    let actual: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == caller_id as u32)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .map(|edge| edge.target)
        .collect();
    assert_eq!(expected.len(), 1);
    assert_eq!(actual, expected);
}

#[test]
fn test_rust_trait_default_nested_function_binds_lexically() {
    let source = r#"
fn helper() {}

trait T {
    fn d(&self) {
        fn helper() { let _ = 1; }
        helper();
    }
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
    let d = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "d")
        .map(|(idx, _)| idx as u32)
        .expect("trait default method node");
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
        .filter(|edge| edge.rel_type == RelType::Calls && edge.source == d)
        .filter(|edge| graph.nodes[edge.target as usize].name.resolve(pool) == "helper")
        .map(|edge| edge.target)
        .collect();
    assert_eq!(targets, vec![nested], "{graph:#?}");
}

#[test]
fn test_rust_closure_bare_call_binds_enclosing_nested_function() {
    let source = r#"
fn helper() {}

fn outer(xs: &[i32]) {
    fn helper() { let _ = 1; }
    xs.iter().for_each(|_| helper());
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
    let nested = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.content_hash == nested_hash)
        .map(|(idx, _)| idx as u32)
        .expect("nested helper node");
    let closure = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool).starts_with("<anonymous:"))
        .map(|(idx, _)| idx as u32)
        .expect("closure node");
    assert!(
        graph.edges.iter().any(|edge| {
            edge.rel_type == RelType::Calls && edge.source == closure && edge.target == nested
        }),
        "{graph:#?}"
    );
}

#[test]
fn test_nested_rust_function_in_module_impl_matches_method_owner_path() {
    let source = r#"
mod services {
    struct A;
    impl A {
        fn new() -> Self {
            fn helper() {}
            helper();
            A
        }
    }
}
"#;
    let local = parse(source);
    let helper = local
        .nodes
        .iter()
        .find(|node| node.name == "helper")
        .expect("helper raw node");
    assert_eq!(helper.owner_class.as_deref(), Some("A::new"), "{local:#?}");

    let graph = graph(source);
    let pool = graph.string_pool.as_slice();
    let new = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "new" && node.owner_class.resolve(pool) == "A")
        .map(|(idx, _)| idx as u32)
        .expect("A::new node");
    let helper = graph
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| node.name.resolve(pool) == "helper")
        .map(|(idx, _)| idx as u32)
        .expect("A::new::helper node");
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.rel_type == RelType::Calls
                && edge.source == new
                && edge.target == helper),
        "{graph:#?}"
    );
}
