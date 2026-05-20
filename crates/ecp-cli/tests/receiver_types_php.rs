//! Integration test: PHP receiver-type binding (A4).
//!
//! Verifies that `$this->method()`, `parent::method()`, `self::method()`,
//! and `static::method()` call sites are bound to the enclosing class so the
//! resolver's Tier 2.5 (qualifier-scoped) lookup can route them correctly.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = PhpProvider::new().unwrap();
    let local = provider
        .parse_file("test.php".as_ref(), src.as_bytes())
        .unwrap();
    local.nodes
}

fn calls_of<'a>(nodes: &'a [RawNode], fn_name: &str) -> &'a [String] {
    nodes
        .iter()
        .find(|n| n.name == fn_name && matches!(n.kind, NodeKind::Method | NodeKind::Function))
        .map(|n| n.calls.as_slice())
        .unwrap_or(&[])
}

#[test]
fn this_arrow_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.php");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "bark");
    assert!(
        calls.iter().any(|c| c == "Dog.greet"),
        "$this->greet() inside Dog::bark must resolve to Dog.greet; got {:?}",
        calls,
    );
}

#[test]
fn parent_colon_colon_binds_to_heritage_class() {
    let src = include_str!("fixtures/receiver_types.php");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "call_parent");
    assert!(
        calls.iter().any(|c| c == "Animal.__construct"),
        "parent::__construct() inside Dog must resolve to Animal.__construct; got {:?}",
        calls,
    );
}

#[test]
fn self_colon_colon_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.php");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "call_self_static");
    assert!(
        calls.iter().any(|c| c == "Dog.helper"),
        "self::helper() inside Dog must resolve to Dog.helper; got {:?}",
        calls,
    );
}

#[test]
fn static_colon_colon_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.php");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "call_self_static");
    assert!(
        calls.iter().filter(|c| *c == "Dog.helper").count() >= 2,
        "both self::helper() and static::helper() must resolve to Dog.helper (2 entries); got {:?}",
        calls,
    );
}

#[test]
fn this_in_different_class_binds_correctly() {
    let src = include_str!("fixtures/receiver_types.php");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "meow");
    assert!(
        calls.iter().any(|c| c == "Cat.speak"),
        "$this->speak() inside Cat::meow must resolve to Cat.speak; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Dog.speak"),
        "Cat::meow must not produce Dog.speak; got {:?}",
        calls,
    );
}
