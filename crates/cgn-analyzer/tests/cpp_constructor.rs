use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CppProvider::new().expect("provider");
    p.parse_file(Path::new("test.cpp"), src.as_bytes())
        .expect("parse")
}

fn has(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    g.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Inline constructor (class body) → Constructor ────────────────────────────

#[test]
fn test_inline_constructor_emits_constructor() {
    let src = "class Foo { public: Foo(int x) {} };\n";
    let g = parse(src);
    assert!(
        has(&g, "Foo", NodeKind::Constructor),
        "inline Foo() must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn test_inline_constructor_not_method() {
    let src = "class Foo { public: Foo(int x) {} };\n";
    let g = parse(src);
    assert!(
        !has(&g, "Foo", NodeKind::Method),
        "inline Foo() must not also emit as Method; nodes: {:#?}",
        g.nodes
    );
}

// ── Out-of-line constructor (Class::Class) → Constructor ─────────────────────

#[test]
fn test_out_of_line_constructor_emits_constructor() {
    let src = "class Bar {};\nBar::Bar() {}\n";
    let g = parse(src);
    assert!(
        has(&g, "Bar", NodeKind::Constructor),
        "out-of-line Bar::Bar() must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}

// ── Negative: regular method stays Method ────────────────────────────────────

#[test]
fn test_regular_method_stays_method() {
    let src = "class Baz { public: Baz() {} void doWork() {} };\n";
    let g = parse(src);
    assert!(
        has(&g, "doWork", NodeKind::Method),
        "doWork must stay Method; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !has(&g, "doWork", NodeKind::Constructor),
        "doWork must not become Constructor; nodes: {:#?}",
        g.nodes
    );
}
