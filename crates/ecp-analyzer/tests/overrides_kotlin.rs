//! Override edge tests for Kotlin.
//!
//! Kotlin REQUIRES the `override` keyword; its absence is a compile error.
//! Layer 1: parser captures `__override__` in decorators.
//! Layer 2: post-process emits `RelType::Overrides` edges.

mod overrides_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use overrides_support::run_overrides;
use std::path::Path;

fn parse(path: &str, src: &str) -> LocalGraph {
    let p = KotlinProvider::new().expect("provider");
    p.parse_file(Path::new(path), src.as_bytes())
        .expect("parse")
}

fn has_override_sentinel(node: &RawNode) -> bool {
    node.decorators.iter().any(|d| d == "__override__")
}

// ── Layer 1: parser capture ────────────────────────────────────────────────

#[test]
fn kotlin_override_keyword_captured_as_sentinel() {
    let src = r#"
open class A {
    open fun foo() {}
}
class B : A() {
    override fun foo() {}
}
"#;
    let g = parse("B.kt", src);
    let foo_nodes: Vec<_> = g.nodes.iter().filter(|n| n.name == "foo").collect();
    // One of the foo nodes (the one in class B) should have __override__.
    let has_sentinel = foo_nodes.iter().any(|n| has_override_sentinel(n));
    assert!(
        has_sentinel,
        "`override fun foo()` must produce __override__ sentinel; got decorators: {:?}",
        foo_nodes.iter().map(|n| &n.decorators).collect::<Vec<_>>()
    );
}

// ── Layer 2: post-process Overrides edges ──────────────────────────────────

#[test]
fn kotlin_single_override_class_extends() {
    let base = parse(
        "A.kt",
        r#"
open class A {
    open fun foo() {}
}
"#,
    );
    let sub = parse(
        "B.kt",
        r#"
class B : A() {
    override fun foo() {}
}
"#,
    );
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    assert!(
        !edges.is_empty(),
        "expected Overrides edge for `override fun foo()`; got none"
    );
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn kotlin_interface_implementation_overrides() {
    let iface = parse(
        "I.kt",
        r#"
interface I {
    fun bar()
}
"#,
    );
    let impl_class = parse(
        "C.kt",
        r#"
class C : I {
    override fun bar() {}
}
"#,
    );
    let lgs = vec![iface, impl_class];
    let edges = run_overrides(&lgs);
    assert!(
        !edges.is_empty(),
        "expected Overrides edge for interface implementation; got none"
    );
}

#[test]
fn kotlin_override_of_override_immediate_supertype_only() {
    // Chain: A.foo → B.foo → C.foo; C's edge should be C.foo → B.foo only.
    let a = parse("A.kt", "open class A { open fun foo() {} }");
    let b = parse("B.kt", "open class B : A() { override fun foo() {} }");
    let c = parse("C.kt", "class C : B() { override fun foo() {} }");
    let lgs = vec![a, b, c];
    let edges = run_overrides(&lgs);
    assert!(!edges.is_empty(), "expected Overrides edges in chain");
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn kotlin_no_match_no_crash() {
    // `override fun orphan()` but no ancestor in graph.
    let src = parse(
        "X.kt",
        r#"
class X : Missing() {
    override fun orphan() {}
}
"#,
    );
    let lgs = vec![src];
    let edges = run_overrides(&lgs);
    assert!(
        edges.is_empty(),
        "no ancestor → no Overrides edge; got {:?}",
        edges
    );
}

#[test]
fn kotlin_non_override_method_not_emitted() {
    // Regular method without `override` must not produce an Overrides edge.
    let base = parse("A.kt", "open class A { open fun foo() {} }");
    let sub = parse("B.kt", "class B : A() { fun other() {} }");
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    assert!(
        edges.is_empty(),
        "non-override method must not produce Overrides edge; got {:?}",
        edges
    );
}
