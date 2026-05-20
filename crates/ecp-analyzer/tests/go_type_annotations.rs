//! Go `type_annotation` capture coverage (Wave 2 / Task D1).
//!
//! Pins that the Go provider now populates `RawNode.type_annotation` for:
//!   * function / method parameter names (Variable nodes)
//!   * struct field names (Property nodes)
//!   * function / method return types (Function / Method nodes)
//!   * top-level `var` declarations with an explicit type (Variable nodes)
//!
//! Short declarations (`n := 1`) intentionally leave `type_annotation=None`
//! because Go's grammar exposes no `type:` field there — `ecp context` should
//! reflect that the type is inferred, not made up.
//!
//! Spec: `docs/specs/2026-05-15-language-coverage-gaps.md` Wave 2 / D1.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = GoProvider::new().expect("GoProvider init");
    let graph = provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

// param_type_* tests removed: formal parameters are no longer emitted as
// Variable nodes (see `fix(analyzer): drop formal_parameter Variable
// emission ...`).

#[test]
fn field_type_basic() {
    let src = "package p\ntype S struct {\n  X int\n}\n";
    let nodes = parse(src);
    let x = find(&nodes, "X", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn field_type_slice() {
    let src = "package p\ntype S struct {\n  Tags []string\n}\n";
    let nodes = parse(src);
    let tags = find(&nodes, "Tags", NodeKind::Property);
    assert_eq!(tags.type_annotation.as_deref(), Some("[]string"));
}

#[test]
fn return_type_single() {
    let src = "package p\nfunc f() int { return 0 }\n";
    let nodes = parse(src);
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("int"));
}

#[test]
fn return_type_multi() {
    let src = "package p\nfunc f() (int, error) { return 0, nil }\n";
    let nodes = parse(src);
    let f = find(&nodes, "f", NodeKind::Function);
    // The grammar captures the whole `(int, error)` parameter_list span.
    assert_eq!(f.type_annotation.as_deref(), Some("(int, error)"));
}

#[test]
fn var_declaration_explicit() {
    let src = "package p\nvar n int = 1\n";
    let nodes = parse(src);
    let n = find(&nodes, "n", NodeKind::Variable);
    assert_eq!(n.type_annotation.as_deref(), Some("int"));
}

#[test]
fn var_declaration_inferred_no_annotation() {
    // Short declarations have no `type:` field — provider emits Variable
    // with type_annotation=None (parity with ref gitnexus oracle).
    let src = "package p\nfunc f() { n := 1; _ = n }\n";
    let nodes = parse(src);
    let n_var = nodes
        .iter()
        .find(|n| n.name == "n" && n.kind == NodeKind::Variable)
        .expect("short-decl `n := 1` must emit a Variable node");
    assert!(
        n_var.type_annotation.is_none(),
        "short-decl Variable must have type_annotation=None; got {:?}",
        n_var.type_annotation,
    );
}

// ─── Multi-name regression (PR #2 review issue #1) ────────────────────────
//
// Pre-fix: `param_name_node` / `var_name_node` were `Option<Node>` and got
// overwritten by each capture in a multi-name match, so only the LAST name
// emitted a Variable node. These tests pin the contract that EVERY name in
// a multi-name decl produces its own Variable carrying the shared type.

// multi_name_param_emits_one_variable_per_name removed (params no longer
// emit Variable nodes); multi_name_var_decl_emits_one_variable_per_name
// kept — that case (`var X, Y int`) still emits two Variables.

#[test]
fn multi_name_var_decl_emits_one_variable_per_name() {
    let src = "package p\nvar X, Y int\n";
    let nodes = parse(src);
    let x = find(&nodes, "X", NodeKind::Variable);
    let y = find(&nodes, "Y", NodeKind::Variable);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
    assert_eq!(y.type_annotation.as_deref(), Some("int"));
    // Go uppercase-first → exported.
    assert!(x.is_exported);
    assert!(y.is_exported);
}
