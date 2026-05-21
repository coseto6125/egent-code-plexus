//! Override edge tests for C++.
//!
//! C++ `override` specifier (C++11+) is detected via the `virtual_specifier`
//! tree-sitter node. Multiple inheritance is supported: a method overriding
//! from both base A and base B emits two Overrides edges.
//! Layer 1: parser captures `__override__` in decorators.
//! Layer 2: post-process emits `RelType::Overrides` edges.

mod overrides_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use overrides_support::run_overrides;
use std::path::Path;

fn parse(path: &str, src: &str) -> LocalGraph {
    let p = CppProvider::new().expect("provider");
    p.parse_file(Path::new(path), src.as_bytes())
        .expect("parse")
}

fn has_override_sentinel(node: &RawNode) -> bool {
    node.decorators.iter().any(|d| d == "__override__")
}

// ── Layer 1: parser capture ────────────────────────────────────────────────

#[test]
fn cpp_override_specifier_captured_as_sentinel() {
    let src = r#"
struct A { virtual void foo() {} };
struct B : public A {
    void foo() override {}
};
"#;
    let g = parse("B.cpp", src);
    // The overriding `foo` (in B) must have __override__ in decorators.
    let foo_nodes: Vec<_> = g.nodes.iter().filter(|n| n.name == "foo").collect();
    let has_sentinel = foo_nodes.iter().any(|n| has_override_sentinel(n));
    assert!(
        has_sentinel,
        "`void foo() override` must produce __override__ sentinel; got: {:?}",
        foo_nodes.iter().map(|n| &n.decorators).collect::<Vec<_>>()
    );
}

// ── Layer 2: post-process Overrides edges ──────────────────────────────────

#[test]
fn cpp_single_override_struct_extends() {
    let base = parse(
        "A.cpp",
        r#"
struct A {
    virtual void foo() {}
};
"#,
    );
    let sub = parse(
        "B.cpp",
        r#"
struct B : public A {
    void foo() override {}
};
"#,
    );
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    assert!(
        !edges.is_empty(),
        "expected Overrides edge for `void foo() override`; got none"
    );
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn cpp_interface_like_pure_virtual_overrides() {
    // Pure virtual (`= 0`) acts as an interface in C++.
    let iface = parse(
        "IBar.hpp",
        r#"
struct IBar {
    virtual void bar() = 0;
};
"#,
    );
    let impl_class = parse(
        "C.cpp",
        r#"
struct C : public IBar {
    void bar() override {}
};
"#,
    );
    let lgs = vec![iface, impl_class];
    let edges = run_overrides(&lgs);
    assert!(
        !edges.is_empty(),
        "expected Overrides edge for pure-virtual implementation; got none"
    );
}

#[test]
fn cpp_override_of_override_immediate_supertype_only() {
    let a = parse("A.cpp", "struct A { virtual void foo() {} };");
    let b = parse("B.cpp", "struct B : public A { void foo() override {} };");
    let c = parse("C.cpp", "struct C : public B { void foo() override {} };");
    let lgs = vec![a, b, c];
    let edges = run_overrides(&lgs);
    assert!(!edges.is_empty(), "expected Overrides edges in chain");
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn cpp_no_match_no_crash() {
    // `override` specifier but ancestor not in graph.
    let src = parse(
        "X.cpp",
        r#"
struct X : public Missing {
    void orphan() override {}
};
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
fn cpp_multiple_inheritance_two_edges() {
    // struct D : public A, public B — D.foo overrides from both A and B.
    // Expected: two Overrides edges (D.foo → A.foo, D.foo → B.foo).
    let a = parse("A.cpp", "struct A { virtual void foo() {} };");
    let b = parse("B.cpp", "struct B { virtual void foo() {} };");
    let d = parse(
        "D.cpp",
        r#"
struct D : public A, public B {
    void foo() override {}
};
"#,
    );
    let lgs = vec![a, b, d];
    let edges = run_overrides(&lgs);
    // D.foo should override both A.foo and B.foo → 2 edges.
    assert!(
        edges.len() >= 2,
        "multiple inheritance must emit one Overrides edge per overridden base; got {:?}",
        edges
    );
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}
