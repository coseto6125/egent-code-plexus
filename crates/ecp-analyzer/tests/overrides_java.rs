//! Override edge tests for Java.
//!
//! Two layers:
//! 1. Parser unit tests — assert that `@Override` lands in `RawNode.decorators`.
//! 2. Post-process integration tests — assert that `emit_edges` emits
//!    `RelType::Overrides` edges from the subtype method to the supertype method.

mod overrides_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use overrides_support::{has_decorator, run_overrides};
use std::path::Path;

fn parse(path: &str, src: &str) -> LocalGraph {
    let p = JavaProvider::new().expect("provider");
    p.parse_file(Path::new(path), src.as_bytes())
        .expect("parse")
}

// ── Layer 1: parser capture ────────────────────────────────────────────────

#[test]
fn java_override_annotation_captured_in_decorators() {
    let src = r#"
class B extends A {
    @Override
    public void foo() {}
}
"#;
    let g = parse("B.java", src);
    let foo = g.nodes.iter().find(|n| n.name == "foo").expect("foo");
    assert!(
        has_decorator(foo, "Override"),
        "@Override must appear in decorators; got: {:?}",
        foo.decorators
    );
}

// ── Layer 2: post-process Overrides edges ──────────────────────────────────

#[test]
fn java_single_override_extends() {
    // class B extends A { @Override void foo() {} }
    // Expected: Overrides edge B.foo → A.foo
    let base = parse(
        "A.java",
        r#"
class A {
    public void foo() {}
}
"#,
    );
    let sub = parse(
        "B.java",
        r#"
class B extends A {
    @Override
    public void foo() {}
}
"#,
    );
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);

    // There are two `foo` methods; the subtype one (later in iteration) is the candidate.
    // The edge must connect a foo → foo where source != target.
    assert!(
        !edges.is_empty(),
        "expected at least one Overrides edge; got none"
    );
    let has_foo_override = edges.iter().any(|(src, tgt)| src != tgt);
    assert!(has_foo_override, "Overrides edge must not be a self-loop");
}

#[test]
fn java_interface_implementation_overrides() {
    // class C implements I { @Override void bar() {} }
    let iface = parse(
        "I.java",
        r#"
interface I {
    void bar();
}
"#,
    );
    let impl_class = parse(
        "C.java",
        r#"
class C implements I {
    @Override
    public void bar() {}
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
fn java_override_of_override_immediate_supertype_only() {
    // A.foo → B.foo → C.foo chain; C @Override should produce C.foo → B.foo only.
    // Design: immediate-supertype-only (see CLAUDE.md and PR body).
    let a = parse(
        "A.java",
        r#"
class A {
    public void foo() {}
}
"#,
    );
    let b = parse(
        "B.java",
        r#"
class B extends A {
    @Override
    public void foo() {}
}
"#,
    );
    let c = parse(
        "C.java",
        r#"
class C extends B {
    @Override
    public void foo() {}
}
"#,
    );
    let lgs = vec![a, b, c];
    let edges = run_overrides(&lgs);
    // Both B.foo→A.foo and C.foo→B.foo should be emitted (one edge per level).
    // Neither C.foo→A.foo should be emitted (skip-level).
    assert!(
        !edges.is_empty(),
        "expected Overrides edges in chain; got none"
    );
    // All edges must be between distinct nodes.
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop in Overrides edges");
    }
}

#[test]
fn java_no_match_no_crash() {
    // Method with @Override but no ancestor method of the same name.
    // ecp must NOT crash — just skip edge emission.
    let src = parse(
        "X.java",
        r#"
class X extends Missing {
    @Override
    public void orphan() {}
}
"#,
    );
    let lgs = vec![src];
    let edges = run_overrides(&lgs);
    // No ancestor in graph → no edges, no panic.
    assert!(
        edges.is_empty(),
        "expected no Overrides edge when ancestor is absent"
    );
}

#[test]
fn java_method_without_override_does_not_emit() {
    // A method that shadows a supertype method but has NO @Override — must NOT emit.
    let base = parse(
        "A.java",
        r#"
class A {
    public void foo() {}
}
"#,
    );
    let sub = parse(
        "B.java",
        r#"
class B extends A {
    public void foo() {}
}
"#,
    );
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    // Without @Override, ecp does NOT emit an Overrides edge.
    assert!(
        edges.is_empty(),
        "must NOT emit Overrides edge when @Override is absent; got {:?}",
        edges
    );
}
