//! C has no anonymous functions, closures, or lambdas — callbacks in C are
//! always passed as NAMED function pointers (`register_cb(handler)` or
//! `register_cb(&handler)`). Therefore the 14-language anonymous-callback
//! feature (emit `<anonymous>` Function nodes so inner calls are not dropped)
//! is a no-op for C: no `queries.scm` capture and no `parser.rs` change is
//! needed or present.
//!
//! This test pins that invariant and guards against future regressions (e.g. a
//! generic query accidentally matching C compound-literal or GNU nested-function
//! extensions). It also verifies that the ordinary call graph for named-function
//! callbacks is unaffected — `process` is recorded as a callee of `handler`,
//! and `register_cb` is recorded as a callee of `setup`.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("CProvider init");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse_file")
}

fn find_fn<'a>(g: &'a LocalGraph, name: &str) -> &'a ecp_core::analyzer::types::RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Function)
        .unwrap_or_else(|| {
            panic!(
                "expected Function node `{name}`, got: {:#?}",
                g.nodes
                    .iter()
                    .map(|n| (&n.name, &n.kind))
                    .collect::<Vec<_>>()
            )
        })
}

/// C named-function-pointer callback: no `<anonymous>` node must be emitted,
/// and the ordinary call graph for `handler` and `setup` must be intact.
#[test]
fn c_named_function_pointer_callback_emits_no_anonymous_node() {
    let src = r#"
void handler(int x) { process(x); }
void setup(void) { register_cb(handler); }
"#;
    let g = parse(src);

    // Primary invariant: no node named "<anonymous>" of any kind.
    assert!(
        !g.nodes.iter().any(|n| n.name.starts_with("<anonymous")),
        "C parser must not emit any `<anonymous>` node; nodes: {:#?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, &n.kind))
            .collect::<Vec<_>>()
    );

    // Secondary invariant: existing call graph is unaffected.
    let handler = find_fn(&g, "handler");
    assert!(
        handler.calls.iter().any(|c| c == "process"),
        "`handler` must record `process` in its calls; got: {:?}",
        handler.calls
    );

    let setup = find_fn(&g, "setup");
    assert!(
        setup.calls.iter().any(|c| c == "register_cb"),
        "`setup` must record `register_cb` in its calls; got: {:?}",
        setup.calls
    );
}

/// Address-of form (`&handler`) is equally a named reference — still no
/// `<anonymous>` node, and `register_cb` still appears in `setup`'s calls.
#[test]
fn c_address_of_callback_emits_no_anonymous_node() {
    let src = r#"
void handler(int x) { process(x); }
void setup(void) { register_cb(&handler); }
"#;
    let g = parse(src);

    assert!(
        !g.nodes.iter().any(|n| n.name.starts_with("<anonymous")),
        "address-of callback must not emit `<anonymous>`; nodes: {:#?}",
        g.nodes
            .iter()
            .map(|n| (&n.name, &n.kind))
            .collect::<Vec<_>>()
    );

    let setup = find_fn(&g, "setup");
    assert!(
        setup.calls.iter().any(|c| c == "register_cb"),
        "`setup` must record `register_cb`; got: {:?}",
        setup.calls
    );
}
