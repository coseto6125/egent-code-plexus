//! Calls inside an anonymous lambda passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `std::for_each(v.begin(), v.end(), [](int x){ process(x); })` at
//! namespace / translation-unit scope produced 0 callers for `process` before
//! this change.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CppProvider::new().expect("provider");
    p.parse_file(Path::new("test.cpp"), src.as_bytes())
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
fn lambda_arg_with_call_attaches_to_anonymous_node() {
    // Multi-line: attach_to_enclosing is row-granular, so the inner call must
    // sit on a row the lambda spans more narrowly than the enclosing function.
    let g = parse(
        "void run(std::vector<int>& v) {\n    std::for_each(v.begin(), v.end(), [](int x){\n        process(x);\n    });\n}",
    );
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn top_level_lambda_callback_attaches_call() {
    // Lambda at translation-unit scope (static initializer / global callback).
    let g = parse(
        "void setup() {\n    register_handler(42, [](int ev){\n        handle(ev);\n    });\n}",
    );
    assert!(
        anonymous_calls(&g).contains(&"handle"),
        "expected handle attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_lambda_emits_no_anonymous_node() {
    // Lambda body contains no call expression — must not bloat the graph.
    let g = parse("void run(std::vector<int>& v) { std::sort(v.begin(), v.end(), [](int a, int b){ return a < b; }); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "lambda without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_pointer_arg_is_not_treated_as_anonymous() {
    // Passing a named function by address is a reference, not a call site —
    // no <anonymous> node must be emitted.
    let g = parse("void setup() { register_handler(42, &myCallback); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn-pointer argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
