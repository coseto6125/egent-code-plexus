//! Dart receiver-type binding (Task A5):
//! `this.method()` / `super.method()` / typed-var `obj.method()` rewrites
//! to `Type.method`. `var`/inferred locals fall back to bare names.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = DartProvider::new().unwrap();
    let local = provider
        .parse_file("test.dart".as_ref(), src.as_bytes())
        .unwrap();
    local.nodes
}

fn calls_for<'a>(nodes: &'a [RawNode], fn_name: &str) -> Vec<&'a [String]> {
    nodes
        .iter()
        .filter(|n| n.name == fn_name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| n.calls.as_slice())
        .collect()
}

fn calls_of<'a>(nodes: &'a [RawNode], fn_name: &str) -> &'a [String] {
    nodes
        .iter()
        .find(|n| n.name == fn_name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| n.calls.as_slice())
        .unwrap_or(&[])
}

#[test]
fn this_dot_method_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.dart");
    let nodes = parse(src);
    let all = calls_for(&nodes, "eat");
    let has_apple_peel = all.iter().any(|c| c.iter().any(|x| x == "Apple.peel"));
    assert!(
        has_apple_peel,
        "Apple.eat should bind this.peel() to Apple.peel; got {:?}",
        all,
    );
}

#[test]
fn super_dot_method_binds_to_extends_heritage() {
    let src = include_str!("fixtures/receiver_types.dart");
    let nodes = parse(src);
    let all = calls_for(&nodes, "eat");
    let has_apple_eat = all.iter().any(|c| c.iter().any(|x| x == "Apple.eat"));
    assert!(
        has_apple_eat,
        "Banana.eat should bind super.eat() to Apple.eat (via extends Apple); got {:?}",
        all,
    );
}

#[test]
fn typed_param_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.dart");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useParam");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "typed param `Apple a` should bind a.peel() to Apple.peel; got {:?}",
        calls,
    );
}

#[test]
fn typed_local_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.dart");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useLocal");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "typed local `Apple a = Apple()` should bind a.peel() to Apple.peel; got {:?}",
        calls,
    );
}

#[test]
fn untyped_var_falls_back_to_bare_name() {
    let src = include_str!("fixtures/receiver_types.dart");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useNoAnnotation");
    assert!(
        calls.iter().any(|c| c == "peel"),
        "`var a = Apple()` (untyped) must fall back to bare `peel`; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Apple.peel"),
        "must not invent Apple.peel without explicit type annotation; got {:?}",
        calls,
    );
}
