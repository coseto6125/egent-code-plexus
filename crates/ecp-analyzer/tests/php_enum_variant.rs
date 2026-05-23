//! NodeKind::EnumVariant emission for PHP 8.1+ `enum` declarations.
//!
//! PHP 8.1 introduced first-class enums with pure (`case A;`) and
//! backed (`case A = 'a';`) forms. Both emit EnumVariant nodes; backing
//! value is metadata, not a separate node.
//! Pre-8.1 class-with-const imitations stay as Class+Const (regression guard).

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse")
}

fn find_kind(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    g.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

fn variants_of<'a>(g: &'a LocalGraph, enum_name: &str) -> Vec<&'a str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant && n.owner_class.as_deref() == Some(enum_name))
        .map(|n| n.name.as_str())
        .collect()
}

fn assert_enum_variants(g: &LocalGraph, enum_name: &str, expected: &[&str]) {
    assert!(
        find_kind(g, enum_name, NodeKind::Enum),
        "Enum node {enum_name:?} missing; nodes: {:?}",
        g.nodes
            .iter()
            .map(|n| (n.name.as_str(), n.kind))
            .collect::<Vec<_>>()
    );

    let mut got = variants_of(g, enum_name);
    got.sort();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort();
    assert_eq!(
        got, want,
        "EnumVariant set for {enum_name:?} mismatch — got={got:?} expected={want:?}"
    );
}

// ── 1. Basic pure enum ────────────────────────────────────────────────────────

#[test]
fn basic_enum_emits_enum_and_variants() {
    let src = "<?php enum Status { case Active; case Inactive; }";
    let g = parse(src);
    assert_enum_variants(&g, "Status", &["Active", "Inactive"]);
}

// ── 2. Backed string enum ─────────────────────────────────────────────────────

#[test]
fn backed_string_enum_emits_enum_and_variants() {
    let src = "<?php enum Color: string { case Red = 'red'; case Blue = 'blue'; }";
    let g = parse(src);
    assert_enum_variants(&g, "Color", &["Red", "Blue"]);
}

// ── 3. Backed int enum ────────────────────────────────────────────────────────

#[test]
fn backed_int_enum_emits_enum_and_variants() {
    let src = "<?php enum Priority: int { case Low = 1; case High = 10; }";
    let g = parse(src);
    assert_enum_variants(&g, "Priority", &["Low", "High"]);
}

// ── 4. Enum with method — method stays Method, case stays EnumVariant ─────────

#[test]
fn enum_with_method_emits_variant_and_method() {
    let src = r#"<?php
enum X {
    case A;
    public function label(): string { return 'a'; }
}"#;
    let g = parse(src);
    assert_enum_variants(&g, "X", &["A"]);
    assert!(
        find_kind(&g, "label", NodeKind::Method),
        "method `label` must be emitted as Method; nodes: {:?}",
        g.nodes
            .iter()
            .map(|n| (n.name.as_str(), n.kind))
            .collect::<Vec<_>>()
    );
    // label must NOT appear as an EnumVariant
    assert!(
        !find_kind(&g, "label", NodeKind::EnumVariant),
        "method `label` must NOT be emitted as EnumVariant"
    );
}

// ── 5. Pre-8.1 class-with-const — regression guard ───────────────────────────

#[test]
fn class_with_const_does_not_emit_enum_variant() {
    let src = r#"<?php
class FakeEnum {
    const A = 1;
    const B = 2;
}"#;
    let g = parse(src);
    // No EnumVariant nodes at all
    let ev_nodes: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert!(
        ev_nodes.is_empty(),
        "class-with-const must NOT emit EnumVariant; got: {ev_nodes:?}"
    );
    // A and B must still be Const
    assert!(find_kind(&g, "A", NodeKind::Const), "A must be Const");
    assert!(find_kind(&g, "B", NodeKind::Const), "B must be Const");
}
