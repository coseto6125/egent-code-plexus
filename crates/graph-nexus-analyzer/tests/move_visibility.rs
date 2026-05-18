//! Visibility checks for the Move provider.
//!
//! Move visibility conventions:
//! * `public fun`            → exported (callable from any module)
//! * `public(friend) fun`    → exported (callable from friend modules)
//! * `public(package) fun`   → exported (callable within package)
//! * `entry fun`             → exported (callable as a transaction entry point)
//! * plain `fun`             → NOT exported (module-private)
//!
//! Struct visibility: `public struct` → exported; plain `struct` → not exported.
//!
//! The tree-sitter-move grammar represents function visibility via a named
//! `modifier` child node on `function_definition`. Structs lack a `modifier`
//! child, so struct visibility is detected via source-text scan.

use graph_nexus_analyzer::move_lang::parser::MoveProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = MoveProvider::new().expect("MoveProvider init");
    let graph = provider
        .parse_file(Path::new("test.move"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

const MODULE_WRAP_OPEN: &str = "module 0x1::T {\n";
const MODULE_WRAP_CLOSE: &str = "\n}";

fn parse_in_module(body: &str) -> Vec<RawNode> {
    let src = format!("{MODULE_WRAP_OPEN}{body}{MODULE_WRAP_CLOSE}");
    parse(&src)
}

// ── function visibility ───────────────────────────────────────────────────────

#[test]
fn public_fun_is_exported() {
    let nodes = parse_in_module("public fun greet(): u64 { 1 }");
    let f = find(&nodes, "greet", NodeKind::Function);
    assert!(f.is_exported, "`public fun greet` must be exported");
}

#[test]
fn private_fun_is_not_exported() {
    let nodes = parse_in_module("fun internal(): u64 { 0 }");
    let f = find(&nodes, "internal", NodeKind::Function);
    assert!(!f.is_exported, "plain `fun internal` must not be exported");
}

#[test]
fn entry_fun_is_exported() {
    let nodes = parse_in_module("entry fun submit(account: &signer) {}");
    let f = find(&nodes, "submit", NodeKind::Function);
    assert!(
        f.is_exported,
        "`entry fun submit` must be exported (tx entry point)"
    );
}

#[test]
fn public_entry_fun_is_exported() {
    let nodes = parse_in_module("public entry fun exec(account: &signer) {}");
    let f = find(&nodes, "exec", NodeKind::Function);
    assert!(f.is_exported, "`public entry fun exec` must be exported");
}

#[test]
fn public_friend_fun_is_exported() {
    let nodes = parse_in_module("public(friend) fun helper(): u64 { 42 }");
    let f = find(&nodes, "helper", NodeKind::Function);
    assert!(
        f.is_exported,
        "`public(friend) fun helper` must be exported"
    );
}

#[test]
fn public_package_fun_is_exported() {
    let nodes = parse_in_module("public(package) fun pkg_fn(): u64 { 0 }");
    let f = find(&nodes, "pkg_fn", NodeKind::Function);
    assert!(
        f.is_exported,
        "`public(package) fun pkg_fn` must be exported"
    );
}

// ── mixed visibility in one module ───────────────────────────────────────────

#[test]
fn mixed_function_visibility() {
    let src = r#"module 0x1::Mixed {
    public fun open_fn(): u64 { 1 }
    fun closed_fn(): u64 { 0 }
    entry fun entry_fn(account: &signer) {}
}"#;
    let nodes = parse(src);

    let open = find(&nodes, "open_fn", NodeKind::Function);
    assert!(open.is_exported, "`public fun open_fn` must be exported");

    let closed = find(&nodes, "closed_fn", NodeKind::Function);
    assert!(
        !closed.is_exported,
        "plain `fun closed_fn` must not be exported"
    );

    let entry = find(&nodes, "entry_fn", NodeKind::Function);
    assert!(entry.is_exported, "`entry fun entry_fn` must be exported");
}

// ── struct visibility ─────────────────────────────────────────────────────────

#[test]
fn public_struct_is_exported() {
    let nodes = parse_in_module("public struct Coin has key { value: u64 }");
    let s = find(&nodes, "Coin", NodeKind::Class);
    assert!(s.is_exported, "`public struct Coin` must be exported");
}

#[test]
fn private_struct_is_not_exported() {
    let nodes = parse_in_module("struct Internal has store { data: u64 }");
    let s = find(&nodes, "Internal", NodeKind::Class);
    assert!(
        !s.is_exported,
        "plain `struct Internal` must not be exported"
    );
}
