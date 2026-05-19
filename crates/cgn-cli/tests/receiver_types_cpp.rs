//! C++ receiver-type binding (Task A5):
//! `this->method()` / `this.method()` → `Class.method`,
//! `Base::method()` → `Base.method`, typed-var `obj.method()` /
//! `obj->method()` → `Type.method`. Falls back to bare for `auto` /
//! template / unannotated cases.

use graph_nexus_analyzer::cpp::parser::CppProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().unwrap();
    let local = provider
        .parse_file("test.cpp".as_ref(), src.as_bytes())
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
fn this_arrow_method_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.cpp");
    let nodes = parse(src);
    // Apple::eat() calls this->peel() → Apple.peel.
    let all = calls_for(&nodes, "eat");
    let has_apple_peel = all.iter().any(|c| c.iter().any(|x| x == "Apple.peel"));
    assert!(
        has_apple_peel,
        "expected an `eat` method to bind this->peel() to Apple.peel; got {:?}",
        all,
    );
}

#[test]
fn qualified_call_base_method_routes_through_qualifier() {
    let src = include_str!("fixtures/receiver_types.cpp");
    let nodes = parse(src);
    // Banana::eat() calls Apple::eat() → Apple.eat.
    let all = calls_for(&nodes, "eat");
    let has_apple_eat = all.iter().any(|c| c.iter().any(|x| x == "Apple.eat"));
    assert!(
        has_apple_eat,
        "expected an `eat` method to bind Apple::eat() to Apple.eat; got {:?}",
        all,
    );
}

#[test]
fn typed_param_binds_member_call() {
    let src = include_str!("fixtures/receiver_types.cpp");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useParam");
    assert!(
        calls.iter().any(|c| c == "Apple.eat"),
        "typed param `Apple a` should bind a.eat() → Apple.eat; got {:?}",
        calls,
    );
}

#[test]
fn typed_local_pointer_and_value_both_bind() {
    let src = include_str!("fixtures/receiver_types.cpp");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useLocal");
    let apple_eat_count = calls.iter().filter(|c| c.as_str() == "Apple.eat").count();
    assert!(
        apple_eat_count >= 2,
        "both `a.eat()` and `p->eat()` should resolve to Apple.eat (≥2 occurrences); got {:?}",
        calls,
    );
}

#[test]
fn auto_typed_local_falls_back_to_bare_name() {
    let src = include_str!("fixtures/receiver_types.cpp");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "useNoAnnotation");
    assert!(
        calls.iter().any(|c| c == "eat"),
        "auto-typed local must fall back to bare `eat`; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Apple.eat"),
        "must not invent Apple.eat without an explicit type annotation; got {:?}",
        calls,
    );
}
