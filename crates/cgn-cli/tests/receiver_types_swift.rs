//! Swift receiver-type binding (Task A5):
//! `self.method()` / `super.method()` / typed-var `obj.method()` rewrites
//! the callee to `Type.method` so the resolver's qualifier-scoped lookup
//! (Tier 2.5) routes to the correct class. Extensions resolve as the
//! extended type's class scope.

use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = SwiftProvider::new().unwrap();
    let local = provider
        .parse_file("test.swift".as_ref(), src.as_bytes())
        .unwrap();
    local.nodes
}

fn calls_of<'a>(nodes: &'a [RawNode], fn_name: &str) -> &'a [String] {
    nodes
        .iter()
        .find(|n| n.name == fn_name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| n.calls.as_slice())
        .unwrap_or(&[])
}

#[test]
fn self_dot_method_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    // Apple.eat calls self.peel() — should be rewritten to Apple.peel.
    // Two `eat` functions exist (Apple, Banana); we want the one inside Apple.
    let apple_eat = nodes
        .iter()
        .find(|n| n.name == "eat" && n.calls.iter().any(|c| c == "Apple.peel"))
        .expect("Apple.eat should have `Apple.peel` in calls");
    assert!(
        !apple_eat.calls.iter().any(|c| c == "peel"),
        "bare `peel` should not coexist with the bound `Apple.peel`; got {:?}",
        apple_eat.calls,
    );
}

#[test]
fn super_dot_method_binds_to_heritage() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    let banana_eat = nodes
        .iter()
        .find(|n| n.name == "eat" && n.calls.iter().any(|c| c == "Fruit.eat"))
        .expect("Banana.eat should have super-bound `Fruit.eat` in calls");
    assert!(
        !banana_eat.calls.iter().any(|c| c == "eat"),
        "bare `eat` should not coexist with `Fruit.eat`; got {:?}",
        banana_eat.calls,
    );
}

#[test]
fn extension_self_binds_to_extended_type() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    // `func slice()` inside `extension Apple` calls self.peel() → Apple.peel.
    let slice_calls = calls_of(&nodes, "slice");
    assert!(
        slice_calls.iter().any(|c| c == "Apple.peel"),
        "extension method's `self` should resolve to extended type Apple; got {:?}",
        slice_calls,
    );
}

#[test]
fn typed_param_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useParam");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "typed param `a: Apple` should bind a.peel() to Apple.peel; got {:?}",
        calls,
    );
}

#[test]
fn typed_local_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useLocal");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "typed local `let a: Apple` should bind a.peel() to Apple.peel; got {:?}",
        calls,
    );
}

#[test]
fn unannotated_var_falls_back_to_bare_name() {
    let src = include_str!("fixtures/receiver_types.swift");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useNoAnnotation");
    assert!(
        calls.iter().any(|c| c == "peel"),
        "without annotation must keep bare `peel`; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c.contains('.')),
        "no qualifier without type info; got {:?}",
        calls,
    );
}
