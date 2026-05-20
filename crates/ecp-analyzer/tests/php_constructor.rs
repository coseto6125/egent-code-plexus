use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse")
}

fn has(g: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    g.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Happy path ───────────────────────────────────────────────────────────────

#[test]
fn test_construct_emits_constructor() {
    let src = "<?php\nclass Foo {\n    public function __construct(int $x) {\n        $this->x = $x;\n    }\n}\n";
    let g = parse(src);
    assert!(
        has(&g, "__construct", NodeKind::Constructor),
        "__construct must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn test_construct_not_also_method() {
    let src = "<?php\nclass Foo {\n    public function __construct(int $x) {}\n}\n";
    let g = parse(src);
    assert!(
        !has(&g, "__construct", NodeKind::Method),
        "__construct must not also emit as Method; nodes: {:#?}",
        g.nodes
    );
}

// ── Negative: regular method stays Method ────────────────────────────────────

#[test]
fn test_regular_method_stays_method() {
    let src = "<?php\nclass Foo {\n    public function __construct() {}\n    public function doWork(): void {}\n}\n";
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
