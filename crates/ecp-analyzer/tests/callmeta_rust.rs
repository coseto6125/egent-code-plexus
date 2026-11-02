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

// ── Rust: indirect-dispatch types are positional, never file-wide ──────────

/// `(caller_name, flags, dispatch_type)` of every `call_metas` row, in order.
fn metas(src: &str) -> Vec<(String, u8, String)> {
    parse_rust("m.rs", src)
        .call_metas
        .iter()
        .map(|m| (m.caller_name.clone(), m.flags, m.dispatch_type.clone()))
        .collect()
}

fn callers(src: &str) -> Vec<String> {
    metas(src).into_iter().map(|(c, ..)| c).collect()
}

#[test]
fn test_rust_indirect_param_types_across_fns_do_not_leak() {
    let src = r#"
pub fn cb() -> i32 { 0 }
fn takes(cb: fn() -> i32) { cb() }
fn plain() { cb() }
fn dyn_user(h: &dyn H) { h.go() }
fn concrete_user(h: &A) { h.go() }
"#;
    // Before the fix the flat name map typed `plain`'s `cb()` as a callback
    // and `concrete_user`'s `h.go()` as dynamic dispatch.
    let got = metas(src);
    assert_eq!(
        got,
        vec![
            (
                "takes".to_string(),
                CallMeta::FLAG_CALLBACK,
                "fn() -> i32".to_string()
            ),
            (
                "dyn_user".to_string(),
                CallMeta::FLAG_DYNAMIC_DISPATCH,
                "dyn H".to_string()
            ),
        ]
    );
}

#[test]
fn test_rust_indirect_method_and_free_fn_same_param_name_only_method_flagged() {
    let src = r#"
struct S;
impl S { fn m(&self, mut cb: fn()) { cb() } }
fn free() { cb() }
"#;
    assert_eq!(callers(src), vec!["m"]);
}

#[test]
fn test_rust_indirect_later_shadowing_let_hides_param_after_it() {
    let src = r#"
fn f(cb: fn()) {
    cb();
    let cb: usize = 0;
    cb();
}
fn g(cb: fn()) {
    cb();
    let cb = 0;
    cb();
}
"#;
    // Only the call before the shadowing `let` is a callback.
    assert_eq!(callers(src), vec!["f", "g"]);
}

#[test]
fn test_rust_indirect_let_initializer_sees_param() {
    let src = r#"
fn f(cb: fn() -> fn()) {
    let cb = cb();
    let _ = cb;
}
"#;
    assert_eq!(callers(src), vec!["f"]);
}

#[test]
fn test_rust_indirect_typed_let_scoped_to_its_block() {
    let src = r#"
fn inside() {
    { let g: fn() = h; g(); }
}
fn outside() {
    { let g: fn() = h; }
    g();
}
fn redeclared() {
    let g: fn() = h;
    let g: u8 = 1;
    g();
}
fn before() {
    g();
    let g: fn() = h;
}
"#;
    // Only the call inside the block that declares the `fn` binding.
    assert_eq!(callers(src), vec!["inside"]);
}

#[test]
fn test_rust_indirect_nested_fn_does_not_see_enclosing_params() {
    let src = r#"
fn outer(cb: fn()) {
    fn inner() { cb() }
    cb();
}
"#;
    // `inner` cannot capture `cb`; its call is direct.
    assert_eq!(callers(src), vec!["outer"]);
}

#[test]
fn test_rust_indirect_nested_fn_own_param_is_tracked() {
    let src = r#"
fn outer(cb: u8) {
    fn inner(cb: fn()) { cb() }
}
"#;
    assert_eq!(callers(src).len(), 1, "got: {:?}", metas(src));
}

#[test]
fn test_rust_indirect_closure_param_is_not_collected() {
    let src = r#"
fn f() {
    let c = |cb: fn()| cb();
}
"#;
    assert!(callers(src).is_empty());
}

#[test]
fn test_rust_indirect_destructured_binding_hides_param() {
    let src = r#"
fn f(cb: fn(), pair: (u8, u8)) {
    let (cb, _x) = pair;
    cb();
}
"#;
    assert!(callers(src).is_empty());
}

#[test]
fn test_rust_indirect_empty_and_bodyless_inputs_yield_no_metas() {
    assert!(metas("").is_empty());
    assert!(metas("   \n").is_empty());
    assert!(metas("trait T { fn go(cb: fn()); }\nfn e() {}").is_empty());
}
