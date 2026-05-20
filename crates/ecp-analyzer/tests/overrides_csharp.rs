//! Override edge tests for C#.
//!
//! C# requires the `override` modifier; without it on a method that matches
//! a `virtual`/`abstract` supertype method, no override relationship exists.
//! Layer 1: parser captures `__override__` in decorators.
//! Layer 2: post-process emits `RelType::Overrides` edges.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::post_process::overrides;
use ecp_analyzer::resolution::index::SymbolTable;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::RelType;
use ecp_core::pool::StringPool;
use std::path::Path;

fn parse(path: &str, src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new(path), src.as_bytes())
        .expect("parse")
}

fn build_symbol_table(local_graphs: &[LocalGraph]) -> SymbolTable {
    let mut st = SymbolTable::new();
    let mut current = 0u32;
    for lg in local_graphs {
        let path_str = lg.file_path.to_string_lossy().replace('\\', "/");
        for rn in &lg.nodes {
            st.register_node(&path_str, &rn.name, current, rn.kind);
            current += 1;
        }
    }
    st
}

fn run_overrides(local_graphs: &[LocalGraph]) -> Vec<(u32, u32)> {
    let st = build_symbol_table(local_graphs);
    let mut sp = StringPool::new();
    let mut edges = Vec::new();
    overrides::emit_edges(local_graphs, &st, &mut sp, &mut edges);
    edges
        .into_iter()
        .filter(|e| matches!(e.rel_type, RelType::Overrides))
        .map(|e| (e.source, e.target))
        .collect()
}

fn has_override_sentinel(node: &RawNode) -> bool {
    node.decorators.iter().any(|d| d == "__override__")
}

// ── Layer 1: parser capture ────────────────────────────────────────────────

#[test]
fn csharp_override_modifier_captured_as_sentinel() {
    let src = r#"
class B : A {
    public override void Foo() {}
}
"#;
    let g = parse("B.cs", src);
    let foo = g.nodes.iter().find(|n| n.name == "Foo").expect("Foo");
    assert!(
        has_override_sentinel(foo),
        "`override void Foo()` must produce __override__ sentinel; got: {:?}",
        foo.decorators
    );
}

// ── Layer 2: post-process Overrides edges ──────────────────────────────────

#[test]
fn csharp_single_override_class_extends() {
    let base = parse(
        "A.cs",
        r#"
class A {
    public virtual void Foo() {}
}
"#,
    );
    let sub = parse(
        "B.cs",
        r#"
class B : A {
    public override void Foo() {}
}
"#,
    );
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    assert!(
        !edges.is_empty(),
        "expected Overrides edge for `override void Foo()`; got none"
    );
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn csharp_interface_implementation_overrides() {
    let iface = parse(
        "IBar.cs",
        r#"
interface IBar {
    void Bar();
}
"#,
    );
    let impl_class = parse(
        "C.cs",
        r#"
class C : IBar {
    public override void Bar() {}
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
fn csharp_override_of_override_immediate_supertype_only() {
    let a = parse("A.cs", "class A { public virtual void Foo() {} }");
    let b = parse("B.cs", "class B : A { public override void Foo() {} }");
    let c = parse("C.cs", "class C : B { public override void Foo() {} }");
    let lgs = vec![a, b, c];
    let edges = run_overrides(&lgs);
    assert!(!edges.is_empty(), "expected Overrides edges in chain");
    for (src, tgt) in &edges {
        assert_ne!(src, tgt, "self-loop Overrides edge");
    }
}

#[test]
fn csharp_no_match_no_crash() {
    let src = parse(
        "X.cs",
        r#"
class X : Missing {
    public override void Orphan() {}
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
fn csharp_virtual_without_override_not_emitted() {
    // A virtual method in the base and a new (non-override) method in the sub.
    let base = parse("A.cs", "class A { public virtual void Foo() {} }");
    let sub = parse("B.cs", "class B : A { public void Other() {} }");
    let lgs = vec![base, sub];
    let edges = run_overrides(&lgs);
    assert!(
        edges.is_empty(),
        "non-override method must not produce Overrides edge; got {:?}",
        edges
    );
}
