use cgn_analyzer::python::parser::PythonProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PythonProvider::new().expect("provider");
    p.parse_file(Path::new("test.py"), src.as_bytes())
        .expect("parse")
}

fn has(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    g.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Happy path ───────────────────────────────────────────────────────────────

#[test]
fn test_init_emits_constructor() {
    let src = "class Foo:\n    def __init__(self, x):\n        self.x = x\n";
    let g = parse(src);
    assert!(
        has(&g, "__init__", NodeKind::Constructor),
        "__init__ must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn test_init_not_also_method() {
    let src = "class Foo:\n    def __init__(self, x):\n        self.x = x\n";
    let g = parse(src);
    assert!(
        !has(&g, "__init__", NodeKind::Method),
        "__init__ must not also emit as Method; nodes: {:#?}",
        g.nodes
    );
}

// ── Negative: regular method stays Method ────────────────────────────────────

#[test]
fn test_regular_method_stays_method() {
    let src = "class Foo:\n    def __init__(self):\n        pass\n    def regular_method(self):\n        pass\n";
    let g = parse(src);
    assert!(
        has(&g, "regular_method", NodeKind::Method),
        "regular_method must stay Method; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !has(&g, "regular_method", NodeKind::Constructor),
        "regular_method must not become Constructor; nodes: {:#?}",
        g.nodes
    );
}

// ── Free function __init__ (module-level) stays Function ────────────────────

#[test]
fn test_module_level_init_stays_function() {
    // A module-level function named __init__ (rare but valid) must not be
    // promoted — it is not a class method.
    let src = "def __init__():\n    pass\n";
    let g = parse(src);
    assert!(
        !has(&g, "__init__", NodeKind::Constructor),
        "module-level __init__ must not emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}
