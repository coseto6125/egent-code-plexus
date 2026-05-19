use cgn_analyzer::kotlin::parser::KotlinProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = KotlinProvider::new().expect("provider");
    p.parse_file(Path::new("Test.kt"), src.as_bytes())
        .expect("parse")
}

// ── Test 1: primary constructor ───────────────────────────────────────────────

#[test]
fn primary_constructor_emits_constructor_kind() {
    let g = parse("class Foo(val x: Int)\n");
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(ctors, vec!["Foo"], "nodes: {:?}", g.nodes);
}

// ── Test 2: two secondary constructors ────────────────────────────────────────

#[test]
fn two_secondary_constructors_emit_two_nodes() {
    let src = "class Foo { constructor(x: Int) {} constructor(y: String) {} }\n";
    let g = parse(src);
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        ctors.len(),
        2,
        "expected 2 Constructor nodes, got {:?}",
        ctors
    );
    assert!(
        ctors.iter().all(|&n| n == "Foo"),
        "all constructors should be named Foo, got {:?}",
        ctors
    );
}

// ── Test 3: primary + secondary in same class ─────────────────────────────────

#[test]
fn primary_and_secondary_in_same_class_emit_two_nodes() {
    let src = "class Foo(val x: Int) { constructor() : this(0) {} }\n";
    let g = parse(src);
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        ctors.len(),
        2,
        "expected 2 Constructor nodes (primary + secondary), got {:?}",
        ctors
    );
    assert!(
        ctors.iter().all(|&n| n == "Foo"),
        "all constructors should be named Foo, got {:?}",
        ctors
    );
}

// ── Test 4: no constructors — zero Constructor nodes ─────────────────────────

#[test]
fn class_with_only_method_emits_no_constructor() {
    let src = "class Foo { fun bar() {} }\n";
    let g = parse(src);
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        ctors.is_empty(),
        "expected no Constructor nodes, got {:?}",
        ctors
    );
}

// ── Test 5: regression — bare class with no constructor declaration ───────────
// Covers the Retrofit-style false-positive: a class with no parens and no
// explicit constructor block must not emit a Constructor node.

#[test]
fn bare_class_without_constructor_emits_no_constructor() {
    let src = "class ApiService {\n    val endpoint: String = \"\"\n    fun fetchData(): String = \"\"\n}\n";
    let g = parse(src);
    let ctors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .map(|n| n.name.as_str())
        .collect();
    assert!(
        ctors.is_empty(),
        "bare class should emit no Constructor nodes, got {:?}",
        ctors
    );
}
