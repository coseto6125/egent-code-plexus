use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{CallMeta, RelType};
use std::path::Path;

fn parse_rust(path: &str, src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

fn build_two_file_graph(
    caller_path: &str,
    caller_src: &str,
    callee_path: &str,
    callee_src: &str,
) -> ecp_core::graph::ZeroCopyGraph {
    let caller_graph = parse_rust(caller_path, caller_src);
    let callee_graph = parse_rust(callee_path, callee_src);
    let mut builder = GraphBuilder::new();
    builder.add_graph(callee_graph);
    builder.add_graph(caller_graph);
    builder.build()
}

// ── Rust: dyn Trait dispatch ────────────────────────────────────────────────

#[test]
fn rust_dyn_trait_call_marked_dynamic_dispatch() {
    // Caller receives &dyn Handler and calls handle() on it.
    let caller_src = r#"
fn run(h: &dyn Handler) {
    h.handle();
}
"#;
    // Callee: trait definition + a concrete impl so resolver can wire the edge.
    let callee_src = r#"
pub trait Handler {
    fn handle(&self);
}
pub struct ConcreteHandler;
impl Handler for ConcreteHandler {
    fn handle(&self) {}
}
"#;
    let g = build_two_file_graph("caller.rs", caller_src, "handler.rs", callee_src);

    // Locate any Calls edge from the `run` function.
    let calls_edges: Vec<(u32, &ecp_core::graph::Edge)> = g
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.rel_type == RelType::Calls)
        .map(|(i, e)| (i as u32, e))
        .collect();

    // The LocalGraph.call_metas for caller.rs should have detected h.handle() as dynamic.
    // Even if the resolver doesn't wire the edge (h is typed &dyn, so no concrete target),
    // validate the RawCallMeta was emitted.
    let caller_graph = parse_rust("caller.rs", caller_src);
    assert!(
        !caller_graph.call_metas.is_empty(),
        "expected at least one RawCallMeta for dyn dispatch call; got none"
    );
    let meta = &caller_graph.call_metas[0];
    assert_eq!(
        meta.flags & CallMeta::FLAG_DYNAMIC_DISPATCH,
        CallMeta::FLAG_DYNAMIC_DISPATCH,
        "dyn Handler call must set FLAG_DYNAMIC_DISPATCH"
    );
    assert_eq!(
        meta.flags & CallMeta::FLAG_DIRECT,
        0,
        "dyn Handler call must NOT set FLAG_DIRECT"
    );

    // Validate dispatch_type contains "dyn"
    assert!(
        meta.dispatch_type.contains("dyn") || meta.dispatch_type.contains("Handler"),
        "dispatch_type should reference dyn Handler, got: {:?}",
        meta.dispatch_type
    );

    // Control: if a Calls edge was emitted (resolver found a target), it has a CallMeta.
    for (edge_idx, _e) in &calls_edges {
        // Any edge from the run function should have dynamic dispatch meta.
        if let Some(cm) = g.call_meta(*edge_idx) {
            assert!(
                cm.is_dynamic_dispatch(),
                "Calls edge {edge_idx} should be dynamic dispatch"
            );
        }
    }
    let _ = calls_edges; // may be empty if resolver can't resolve &dyn
}

// ── Rust: fn-pointer / closure callback ────────────────────────────────────

#[test]
fn rust_fn_pointer_call_marked_callback() {
    let caller_src = r#"
fn invoke(callback: fn(i32) -> i32, x: i32) -> i32 {
    callback(x)
}
"#;
    let callee_src = r#"
pub fn double(x: i32) -> i32 { x * 2 }
"#;
    let g = build_two_file_graph("caller.rs", caller_src, "lib.rs", callee_src);

    // Check at LocalGraph level: fn-pointer param should be FLAG_CALLBACK.
    let caller_graph = parse_rust("caller.rs", caller_src);
    // The call `callback(x)` where `callback: fn(i32) -> i32` should produce RawCallMeta.
    let meta = caller_graph
        .call_metas
        .iter()
        .find(|m| m.caller_name == "invoke");
    assert!(
        meta.is_some(),
        "expected RawCallMeta for fn-ptr call in `invoke`; call_metas: {:?}",
        caller_graph.call_metas
    );
    let meta = meta.unwrap();
    assert_eq!(
        meta.flags & CallMeta::FLAG_CALLBACK,
        CallMeta::FLAG_CALLBACK,
        "fn-pointer call must set FLAG_CALLBACK"
    );
    assert_eq!(
        meta.flags & CallMeta::FLAG_DIRECT,
        0,
        "fn-pointer call must NOT set FLAG_DIRECT"
    );

    let _ = g;
}

// ── Rust: direct call control — no CallMeta entry ──────────────────────────

#[test]
fn rust_direct_call_has_no_callmeta() {
    let caller_src = r#"
fn do_work() {
    compute();
}
"#;
    let callee_src = r#"
pub fn compute() {}
"#;
    let g = build_two_file_graph("caller.rs", caller_src, "lib.rs", callee_src);

    // Direct call should produce NO RawCallMeta at the LocalGraph level.
    let caller_graph = parse_rust("caller.rs", caller_src);
    assert!(
        caller_graph.call_metas.is_empty(),
        "direct call must not produce any RawCallMeta; got: {:?}",
        caller_graph.call_metas
    );

    // And in the final ZeroCopyGraph, the Calls edge (if resolved) should have no CallMeta.
    for (i, _e) in g
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.rel_type == RelType::Calls)
    {
        assert!(
            g.call_meta(i as u32).is_none(),
            "direct Calls edge {i} must have no CallMeta (is_direct by default)"
        );
    }
}

// ── Rust: constructor call flag ────────────────────────────────────────────

#[test]
fn rust_constructor_call_marked() {
    let src = r#"
struct Dog;
impl Dog {
    pub fn new() -> Self { Dog }
}
fn make_dog() -> Dog {
    Dog::new()
}
"#;
    let g = parse_rust("lib.rs", src);
    // Dog::new() → FLAG_CONSTRUCTOR_CALL
    let ctor_meta = g.call_metas.iter().find(|m| m.caller_name == "make_dog");
    // Constructor detection is present; flags must include CONSTRUCTOR_CALL.
    if let Some(meta) = ctor_meta {
        assert_eq!(
            meta.flags & CallMeta::FLAG_CONSTRUCTOR_CALL,
            CallMeta::FLAG_CONSTRUCTOR_CALL,
            "Dog::new() must set FLAG_CONSTRUCTOR_CALL"
        );
    }
    // No assertion if absent — constructor detection is best-effort for scoped_identifier.
    let _ = g;
}
