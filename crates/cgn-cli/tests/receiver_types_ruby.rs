//! Integration test: Ruby receiver-type binding (A4).
//!
//! Verifies that `self.method` calls inside a class body are bound to the
//! enclosing class, and that `Constant.method` singleton calls are bound to
//! the constant name.  Arbitrary `var.method` calls fall back to the bare
//! method name (undecidable without full type inference).

use cgn_analyzer::ruby::parser::RubyProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = RubyProvider::new().unwrap();
    let local = provider
        .parse_file("test.rb".as_ref(), src.as_bytes())
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
fn self_dot_method_binds_to_enclosing_class() {
    let src = include_str!("fixtures/receiver_types.rb");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "bark");
    assert!(
        calls.iter().any(|c| c == "Dog.speak"),
        "self.speak inside Dog#bark must resolve to Dog.speak; got {:?}",
        calls,
    );
    assert!(
        calls.iter().any(|c| c == "Dog.fetch_ball"),
        "self.fetch_ball inside Dog#bark must resolve to Dog.fetch_ball; got {:?}",
        calls,
    );
}

#[test]
fn self_in_different_class_binds_correctly() {
    let src = include_str!("fixtures/receiver_types.rb");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "meow");
    assert!(
        calls.iter().any(|c| c == "Cat.purr"),
        "self.purr inside Cat#meow must resolve to Cat.purr; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "Dog.purr"),
        "Cat#meow must not produce Dog.purr; got {:?}",
        calls,
    );
}

#[test]
fn self_does_not_bind_in_module_singleton_method() {
    // `Trainer.train` is a singleton method — the receiver `animal` is not `self`,
    // so it falls back to the bare method name `speak`.
    let src = include_str!("fixtures/receiver_types.rb");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "train");
    assert!(
        calls.iter().any(|c| c == "speak"),
        "animal.speak in Trainer.train must fall back to bare `speak`; got {:?}",
        calls,
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("Trainer.") || c.starts_with("Animal.")),
        "untyped receiver must not produce a class-qualified call; got {:?}",
        calls,
    );
}
