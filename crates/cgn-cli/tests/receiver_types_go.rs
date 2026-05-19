//! Go receiver-type binding (Task A3):
//! `func (d *Dog) Bark()` introduces `d → Dog`, so `d.Fetch()` rewrites to
//! `Dog.Fetch`. Param/var/short-var with explicit type bind the same way.
//! Without a known type, fall back to the bare method name.

use graph_nexus_analyzer::go::parser::GoProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = GoProvider::new().unwrap();
    let local = provider
        .parse_file("test.go".as_ref(), src.as_bytes())
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
fn receiver_var_binds_to_receiver_type() {
    let src = include_str!("fixtures/receiver_types.go");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "Bark");
    assert!(
        calls.iter().any(|c| c == "Dog.Fetch"),
        "Dog.Bark's `d.Fetch()` should bind to Dog.Fetch; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Fetch"),
        "bare `Fetch` should not coexist with bound `Dog.Fetch`; got {:?}",
        calls,
    );
}

#[test]
fn pointer_param_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.go");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "UseParam");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "param `d *Dog`'s d.Bark() should bind to Dog.Bark; got {:?}",
        calls,
    );
}

#[test]
fn var_declaration_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.go");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "UseVar");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "`var d Dog`'s d.Bark() should bind to Dog.Bark; got {:?}",
        calls,
    );
}

#[test]
fn short_var_composite_literal_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.go");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "UseShortVar");
    assert!(
        calls.iter().any(|c| c == "Dog.Bark"),
        "`d := Dog{{...}}`'s d.Bark() should bind to Dog.Bark; got {:?}",
        calls,
    );
}

#[test]
fn short_var_from_function_call_falls_back_to_bare() {
    let src = include_str!("fixtures/receiver_types.go");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "UseNoType");
    assert!(
        calls.iter().any(|c| c == "Bark"),
        "`d := makeDog()` has no type info — d.Bark() must fall back to bare `Bark`; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Dog.Bark"),
        "no type info — must NOT speculatively bind to Dog.Bark; got {:?}",
        calls,
    );
}
