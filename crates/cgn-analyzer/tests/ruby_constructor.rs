use cgn_analyzer::ruby::parser::RubyProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RubyProvider::new().expect("provider");
    p.parse_file(Path::new("test.rb"), src.as_bytes())
        .expect("parse")
}

fn has(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    g.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Happy path ───────────────────────────────────────────────────────────────

#[test]
fn test_initialize_emits_constructor() {
    let src = "class Foo\n  def initialize(x)\n    @x = x\n  end\nend\n";
    let g = parse(src);
    assert!(
        has(&g, "initialize", NodeKind::Constructor),
        "initialize must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn test_initialize_not_also_method() {
    let src = "class Foo\n  def initialize(x)\n    @x = x\n  end\nend\n";
    let g = parse(src);
    assert!(
        !has(&g, "initialize", NodeKind::Method),
        "initialize must not also emit as Method; nodes: {:#?}",
        g.nodes
    );
}

// ── Negative: regular method stays Method ────────────────────────────────────

#[test]
fn test_regular_method_stays_method() {
    let src = "class Foo\n  def initialize\n  end\n  def greet\n  end\nend\n";
    let g = parse(src);
    assert!(
        has(&g, "greet", NodeKind::Method),
        "greet must stay Method; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !has(&g, "greet", NodeKind::Constructor),
        "greet must not become Constructor; nodes: {:#?}",
        g.nodes
    );
}
