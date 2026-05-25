//! Calls inside an anonymous lambda passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` when no named enclosing scope exists.
//!
//! Repro: `executor.submit(() -> process())` at class-body level produced 0
//! callers for `process` before this change because the lambda body's calls
//! had no named host.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new("test.java"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

/// Expression-body lambda (`x -> expr`) in argument position — inner call
/// must be attached to an `<anonymous>` Function node.
#[test]
fn expression_body_lambda_callback_attaches_call() {
    let g = parse(
        r#"
class App {
    void setup() {
        list.forEach(x -> process(x));
    }
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

/// Block-body lambda (`() -> { ... }`) in argument position — inner call
/// must also be attached to the `<anonymous>` node.
#[test]
fn block_body_lambda_callback_attaches_call() {
    let g = parse(
        r#"
class App {
    void setup() {
        executor.submit(() -> {
            run();
        });
    }
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"run"),
        "expected run attached to <anonymous> (block body), nodes: {:?}",
        g.nodes
    );
}

/// Pure-transform lambda with no call (`x -> x + 1`) must not emit an
/// `<anonymous>` node — only call-bearing lambdas get one.
#[test]
fn empty_lambda_emits_no_anonymous_node() {
    let g = parse(
        r#"
class App {
    void setup() {
        list.stream().map(x -> x + 1).collect(java.util.stream.Collectors.toList());
    }
}
"#,
    );
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "lambda without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

/// Passing a named method reference as an argument must not produce an
/// `<anonymous>` node — method references are not lambda_expression nodes.
#[test]
fn method_reference_arg_emits_no_anonymous_node() {
    let g = parse(
        r#"
class App {
    void handle(String s) { }
    void setup() {
        list.forEach(this::handle);
    }
}
"#,
    );
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "method reference must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
