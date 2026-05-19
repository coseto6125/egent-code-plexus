//! NodeKind::Trait and NodeKind::Enum emission for the PHP parser.

use cgn_analyzer::php::parser::PhpProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse")
}

fn count_kind(g: &LocalGraph, kind: NodeKind) -> usize {
    g.nodes.iter().filter(|n| n.kind == kind).count()
}

fn names_of_kind(g: &LocalGraph, kind: NodeKind) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

// ── Trait ────────────────────────────────────────────────────────────────────

#[test]
fn trait_emits_one_trait_node() {
    let src = "<?php trait Greetable { public function hello() {} }";
    let g = parse(src);
    let traits = names_of_kind(&g, NodeKind::Trait);
    assert_eq!(traits.len(), 1, "expected 1 Trait, got: {traits:?}");
    assert_eq!(traits[0], "Greetable");
}

#[test]
fn trait_method_emits_method_node() {
    let src = "<?php trait Greetable { public function hello() {} }";
    let g = parse(src);
    let methods = count_kind(&g, NodeKind::Method);
    assert_eq!(methods, 1, "expected 1 Method inside trait, got {methods}");
}

// ── Enum ─────────────────────────────────────────────────────────────────────

#[test]
fn plain_enum_emits_one_enum_node() {
    let src = "<?php enum Status { case Active; case Inactive; }";
    let g = parse(src);
    let enums = names_of_kind(&g, NodeKind::Enum);
    assert_eq!(enums.len(), 1, "expected 1 Enum, got: {enums:?}");
    assert_eq!(enums[0], "Status");
}

#[test]
fn backed_enum_int_emits_one_enum_node() {
    let src = "<?php enum Status: int { case Active = 1; case Inactive = 2; }";
    let g = parse(src);
    let enums = names_of_kind(&g, NodeKind::Enum);
    assert_eq!(enums.len(), 1, "expected 1 Enum for backed enum, got: {enums:?}");
    assert_eq!(enums[0], "Status");
}
