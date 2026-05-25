//! Calls inside an anonymous closure passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `v.iter().for_each(|x| process(x))` at module top level produced
//! 0 callers for `process` before this change because no named enclosing
//! scope existed to host the call.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse_rs(src: &str) -> LocalGraph {
    let p = RustProvider::new().expect("provider");
    p.parse_file(Path::new("test.rs"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

#[test]
fn closure_arg_with_call_attaches_to_anonymous_node() {
    // Multi-line: attach_to_enclosing is row-granular, so the inner call must
    // sit on a row the closure spans more narrowly than the enclosing fn.
    let g = parse_rs("fn m() {\n    v.iter().for_each(|x| {\n        process(x);\n    });\n}");
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn thread_spawn_closure_attaches_call_to_anonymous_node() {
    let g = parse_rs("fn m() {\n    std::thread::spawn(|| {\n        run();\n    });\n}");
    assert!(
        anonymous_calls(&g).contains(&"run"),
        "expected run attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_closure_emits_no_anonymous_node() {
    // `|x| x + 1` contains no call_expression — must not emit a node.
    let g = parse_rs("fn m() { v.iter().map(|x| x + 1); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "closure without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_path_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function by path (`process`) is a reference, not a
    // closure literal — no `closure_expression` node, so no `<anonymous>` node.
    let g = parse_rs("fn m() { v.iter().for_each(process); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn path argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
