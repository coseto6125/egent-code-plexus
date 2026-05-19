//! C receiver-type binding (Task A5):
//! C has no methods. We emulate them via the convention
//! `void op(struct T *self, ...)`: when a function's first param is a
//! pointer to a struct/typedef and named `self`/`this`/`me`, calls to
//! that function are rewritten to `T.op` so the resolver's Tier 2.5
//! qualifier-scoped lookup can route correctly.
//!
//! Conservative on purpose — names outside the recognized receiver set
//! and free functions fall back to bare names.

use cgn_analyzer::c::parser::CProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawNode;
use cgn_core::graph::NodeKind;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CProvider::new().unwrap();
    let local = provider
        .parse_file("test.c".as_ref(), src.as_bytes())
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
fn receiver_convention_self_binds_method() {
    let src = include_str!("fixtures/receiver_types.c");
    let nodes = parse(src);
    let main_calls = calls_of(&nodes, "main");
    assert!(
        main_calls.iter().any(|c| c == "Calculator.calc_add"),
        "calc_add(struct Calculator *self,...) should be bound to Calculator.calc_add; got {:?}",
        main_calls,
    );
}

#[test]
fn receiver_convention_this_also_recognized() {
    let src = include_str!("fixtures/receiver_types.c");
    let nodes = parse(src);
    let main_calls = calls_of(&nodes, "main");
    assert!(
        main_calls.iter().any(|c| c == "Calculator.calc_get"),
        "calc_get(struct Calculator *this,...) should be bound to Calculator.calc_get; got {:?}",
        main_calls,
    );
}

#[test]
fn non_receiver_first_param_name_falls_back_to_bare() {
    let src = include_str!("fixtures/receiver_types.c");
    let nodes = parse(src);
    let main_calls = calls_of(&nodes, "main");
    // calc_reset's first param name is `x` — not in RECEIVER_NAMES.
    // Conservative: leave the call as bare `calc_reset`.
    assert!(
        main_calls.iter().any(|c| c == "calc_reset"),
        "calc_reset has non-receiver first-param name → bare callee; got {:?}",
        main_calls,
    );
    assert!(
        !main_calls.iter().any(|c| c == "Calculator.calc_reset"),
        "must not invent a binding when the convention isn't met; got {:?}",
        main_calls,
    );
}

#[test]
fn free_function_stays_bare() {
    let src = include_str!("fixtures/receiver_types.c");
    let nodes = parse(src);
    let main_calls = calls_of(&nodes, "main");
    assert!(
        main_calls.iter().any(|c| c == "add"),
        "free function `add(int,int)` must stay bare; got {:?}",
        main_calls,
    );
    assert!(
        !main_calls
            .iter()
            .any(|c| c.contains('.') && c.ends_with(".add")),
        "no qualifier without receiver shape; got {:?}",
        main_calls,
    );
}
