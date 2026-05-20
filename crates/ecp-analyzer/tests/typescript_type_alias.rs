//! Verifies that TypeScript `type X = ...` aliases emit `NodeKind::Typedef`.

use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse")
}

fn typedefs(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Typedef)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn plain_type_alias_emits_typedef() {
    let g = parse("type Foo = string;");
    let defs = typedefs(&g);
    assert_eq!(defs.len(), 1, "nodes: {:?}", g.nodes);
    assert_eq!(defs[0], "Foo");
}

#[test]
fn exported_type_alias_emits_typedef() {
    let g = parse("export type Foo = number;");
    let defs = typedefs(&g);
    assert_eq!(defs.len(), 1, "nodes: {:?}", g.nodes);
    assert_eq!(defs[0], "Foo");
}

#[test]
fn generic_type_alias_emits_typedef() {
    let g = parse("type Tuple<A, B> = [A, B];");
    let defs = typedefs(&g);
    assert_eq!(defs.len(), 1, "nodes: {:?}", g.nodes);
    assert_eq!(defs[0], "Tuple");
}

#[test]
fn union_type_alias_emits_typedef() {
    let g = parse(r#"type Union = "a" | "b";"#);
    let defs = typedefs(&g);
    assert_eq!(defs.len(), 1, "nodes: {:?}", g.nodes);
    assert_eq!(defs[0], "Union");
}
