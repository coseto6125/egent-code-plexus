//! NodeKind::Namespace emission for the PHP parser.

use graph_nexus_analyzer::php::parser::PhpProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = PhpProvider::new().expect("provider");
    p.parse_file(Path::new("test.php"), src.as_bytes())
        .expect("parse")
}

fn namespaces(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Namespace)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn simple_namespace_emits_one_node() {
    let src = "<?php namespace App; class C {}";
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got: {ns:?}");
    assert_eq!(ns[0], "App");
}

#[test]
fn qualified_namespace_preserves_backslash_segments() {
    let src = "<?php namespace App\\Http\\Controllers; class FooController {}";
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got: {ns:?}");
    assert_eq!(ns[0], "App\\Http\\Controllers");
}

#[test]
fn bracketed_namespace_emits_one_node() {
    let src = "<?php namespace App { class C {} }";
    let g = parse(src);
    let ns = namespaces(&g);
    assert_eq!(ns.len(), 1, "expected 1 Namespace, got: {ns:?}");
    assert_eq!(ns[0], "App");
}
