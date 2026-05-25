//! Calls inside an anonymous callback passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` when no named enclosing scope exists.
//!
//! Repro: `items.ForEach(x => Process(x))` at class-body level produced 0
//! callers for `Process` before this change because the lambda body's calls
//! had no named host.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

/// Expression-body lambda (`x => Foo(x)`) in argument position — inner call
/// must be attached to an `<anonymous>` Function node.
#[test]
fn expression_body_lambda_callback_attaches_call() {
    let g = parse(
        r#"
class App {
    void Setup() {
        items.ForEach(x => Process(x));
    }
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"Process"),
        "expected Process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

/// Block-body lambda (`() => { ... }`) in argument position — inner call
/// must also be attached to the `<anonymous>` node.
#[test]
fn block_body_lambda_callback_attaches_call() {
    let g = parse(
        r#"
class App {
    void Setup() {
        Task.Run(() => {
            Work();
        });
    }
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"Work"),
        "expected Work attached to <anonymous> (block body), nodes: {:?}",
        g.nodes
    );
}

/// `delegate { ... }` anonymous method expression — inner call must attach.
#[test]
fn anonymous_method_expression_attaches_call() {
    let g = parse(
        r#"
class App {
    void Setup() {
        Task.Run(delegate { HandleClick(); });
    }
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"HandleClick"),
        "expected HandleClick attached to <anonymous> (delegate), nodes: {:?}",
        g.nodes
    );
}

/// Pure-transform lambda with no call (`x => x.Name`) must not emit an
/// `<anonymous>` node — only call-bearing closures get one.
#[test]
fn empty_lambda_emits_no_anonymous_node() {
    let g = parse(
        r#"
class App {
    void Setup() {
        var names = items.Select(x => x.Name).ToList();
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

/// Passing a named method group as an argument must not produce an
/// `<anonymous>` node — method groups are identifiers, not lambda nodes.
#[test]
fn named_method_group_arg_emits_no_anonymous_node() {
    let g = parse(
        r#"
class App {
    void Handle(string s) { }
    void Setup() {
        items.ForEach(Handle);
    }
}
"#,
    );
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named method group must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
