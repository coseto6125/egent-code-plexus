//! State-variable visibility checks for the Solidity provider.
//!
//! Solidity state variables carry an explicit visibility keyword
//! (`public`, `private`, `internal`, `external`). The provider must
//! translate that into `RawNode.is_exported`:
//!
//! * `public` / `external` → `is_exported = true`
//! * `private` / `internal` → `is_exported = false`
//! * Missing keyword         → `is_exported = false` (Solidity defaults to
//!   `internal`)
//!
//! Covers Matrix B3 (visibility row) from
//! `docs/specs/2026-05-15-matrix-optimization-opportunities.md`.

use cgn_analyzer::solidity::parser::SolidityProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = SolidityProvider::new().expect("SolidityProvider init");
    let graph = provider
        .parse_file(Path::new("test.sol"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn public_state_var_is_exported() {
    let nodes = parse("contract C { uint256 public foo; }");
    let foo = find(&nodes, "foo", NodeKind::Const);
    assert!(foo.is_exported, "`public` state var `foo` must be exported");
}

#[test]
fn private_state_var_is_not_exported() {
    let nodes = parse("contract C { uint256 private bar; }");
    let bar = find(&nodes, "bar", NodeKind::Const);
    assert!(
        !bar.is_exported,
        "`private` state var `bar` must not be exported"
    );
}

#[test]
fn internal_state_var_is_not_exported() {
    let nodes = parse("contract C { uint256 internal baz; }");
    let baz = find(&nodes, "baz", NodeKind::Const);
    assert!(
        !baz.is_exported,
        "`internal` state var `baz` must not be exported"
    );
}

#[test]
fn external_state_var_when_grammar_accepts() {
    // `external` is not a semantically valid visibility for state vars in
    // Solidity, but `tree-sitter-solidity`'s grammar still permits it. We
    // only assert behaviour when the parser actually emits the node — if a
    // future grammar version rejects it, the symbol simply won't appear.
    let nodes = parse("contract C { uint256 external x; }");
    if let Some(x) = nodes
        .iter()
        .find(|n| n.name == "x" && n.kind == NodeKind::Const)
    {
        assert!(
            x.is_exported,
            "`external` state var `x` should be treated as exported when emitted"
        );
    }
}

#[test]
fn default_state_var_is_not_exported() {
    // Solidity's implicit default for state vars is `internal`.
    let nodes = parse("contract C { uint256 stuff; }");
    let stuff = find(&nodes, "stuff", NodeKind::Const);
    assert!(
        !stuff.is_exported,
        "default (no keyword) state var must not be exported"
    );
}

#[test]
fn multiple_state_vars_each_carry_own_visibility() {
    let src = r#"
        contract C {
            uint256 public pubVar;
            uint256 private privVar;
            uint256 internal intVar;
            uint256 plainVar;
        }
    "#;
    let nodes = parse(src);

    let pub_var = find(&nodes, "pubVar", NodeKind::Const);
    assert!(pub_var.is_exported, "`pubVar` must be exported");

    let priv_var = find(&nodes, "privVar", NodeKind::Const);
    assert!(!priv_var.is_exported, "`privVar` must not be exported");

    let int_var = find(&nodes, "intVar", NodeKind::Const);
    assert!(!int_var.is_exported, "`intVar` must not be exported");

    let plain_var = find(&nodes, "plainVar", NodeKind::Const);
    assert!(
        !plain_var.is_exported,
        "default `plainVar` must not be exported"
    );
}

#[test]
fn events_remain_exported() {
    // Events are captured as `@const` (no visibility); they must keep the
    // pre-existing `is_exported = true` behaviour.
    let nodes = parse("contract C { event Transfer(address from); }");
    let ev = find(&nodes, "Transfer", NodeKind::Const);
    assert!(ev.is_exported, "events must remain exported");
}
