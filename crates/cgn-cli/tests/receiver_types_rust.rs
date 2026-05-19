//! Rust receiver-type binding (Task A3):
//! `self.method()` inside `impl Dog` rewrites to `Dog.method`. Typed param /
//! `let x: Dog` bind locals to the type. `Dog::new()` already-qualified scoped
//! paths are kept as-is. Inferred `let x = Dog::new()` (no annotation) falls
//! back to bare method name.

use cgn_analyzer::rust::parser::RustProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = RustProvider::new().unwrap();
    let local = provider
        .parse_file("test.rs".as_ref(), src.as_bytes())
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
fn self_dot_method_binds_to_impl_type() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "bark");
    assert!(
        calls.iter().any(|c| c == "Dog.fetch"),
        "Dog::bark's self.fetch() should bind to Dog.fetch; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "fetch"),
        "bare `fetch` should not coexist with bound `Dog.fetch`; got {:?}",
        calls,
    );
}

#[test]
fn trait_impl_self_resolves_to_concrete_type() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    // `impl Animal for Dog { fn speak() { self.bark() } }` → self → Dog.
    // There are two `speak` candidates if the trait stub also has one; only the
    // impl variant has a body with a call.
    let speak_with_call = nodes
        .iter()
        .find(|n| n.name == "speak" && !n.calls.is_empty())
        .expect("Dog::speak impl should have a call");
    assert!(
        speak_with_call.calls.iter().any(|c| c == "Dog.bark"),
        "trait-impl self should bind to concrete Dog; got {:?}",
        speak_with_call.calls,
    );
}

#[test]
fn typed_param_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_param");
    assert!(
        calls.iter().any(|c| c == "Dog.bark"),
        "param `d: &Dog`'s d.bark() should bind to Dog.bark; got {:?}",
        calls,
    );
}

#[test]
fn typed_let_binding_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_let_typed");
    assert!(
        calls.iter().any(|c| c == "Dog.bark"),
        "`let d: Dog`'s d.bark() should bind to Dog.bark; got {:?}",
        calls,
    );
}

#[test]
fn unannotated_let_falls_back_to_bare() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_let_inferred");
    assert!(
        calls.iter().any(|c| c == "bark"),
        "`let d = Dog::new()` has no annotation — d.bark() must fall back to bare `bark`; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Dog.bark"),
        "no type annotation — must NOT speculatively bind to Dog.bark; got {:?}",
        calls,
    );
}

#[test]
fn scoped_path_call_preserved() {
    let src = include_str!("fixtures/receiver_types.rs");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_scoped_path");
    assert!(
        calls.iter().any(|c| c == "Dog::new"),
        "`Dog::new()` should stay as scoped path `Dog::new`; got {:?}",
        calls,
    );
}
