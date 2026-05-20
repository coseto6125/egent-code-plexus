//! PHP `const NAME = ...` declarations must emit as Const, both inside a
//! class body (`class Foo { const BAR = 1; }`) and at module-level
//! (`const VERSION = '1.0';`). Before this capture every PHP class
//! constant was missing from the graph — 49 unpaired ref_over Const
//! entries on the Laravel fixture (CREATED_AT, INVALID_TOKEN, etc.).

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("PhpProvider init");
    p.parse_file(Path::new("t.php"), src.as_bytes())
        .expect("parse_file")
}

fn consts(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect()
}

#[test]
fn class_const_emits_const() {
    let g = parse("<?php\nclass Model {\n    const CREATED_AT = 'created_at';\n}\n");
    let cs = consts(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "CREATED_AT");
}

#[test]
fn private_class_const_emits_const() {
    let g = parse("<?php\nclass Foo {\n    private const SECRET = 'x';\n}\n");
    let cs = consts(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "SECRET");
}

#[test]
fn multi_const_declaration_emits_one_per_element() {
    // `const A = 1, B = 2;` is a single const_declaration with 2 const_element
    let g = parse("<?php\nclass Foo {\n    const A = 1, B = 2;\n}\n");
    let names: Vec<&str> = consts(&g).iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
}

#[test]
fn module_level_const_emits_const() {
    let g = parse("<?php\nconst VERSION = '1.0';\n");
    let cs = consts(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "VERSION");
}

#[test]
fn interface_const_emits_const() {
    let g =
        parse("<?php\ninterface PasswordBroker {\n    const INVALID_TOKEN = 'invalid-token';\n}\n");
    let cs = consts(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "INVALID_TOKEN");
}

#[test]
fn class_without_const_emits_no_const() {
    // Regression guard: classes without const declarations must not leak Const nodes.
    let g = parse("<?php\nclass Foo {\n    public $bar;\n    function baz() {}\n}\n");
    let cs = consts(&g);
    assert!(cs.is_empty(), "no const declared but got {:?}", cs);
}
