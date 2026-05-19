//! Nim "Named" dimension — alias / typedef detection.
//!
//! Emits `NodeKind::Typedef` for:
//!   - `type Score = int`          (simple alias)
//!   - `type Cb = proc(x: int): int` (alias to proc type)
//!   - `type Pair = tuple[a, b: int]` (tuple alias)
//!
//! Does NOT emit Typedef for `type Person = object` (stays Class).

use cgn_analyzer::nim::parser::NimProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind)> {
    let provider = NimProvider::new().expect("NimProvider::new");
    let graph = provider
        .parse_file(Path::new("t.nim"), src.as_bytes())
        .expect("parse_file");
    graph.nodes.iter().map(|n| (n.name.clone(), n.kind)).collect()
}

fn find_node<'a>(nodes: &'a [(String, NodeKind)], name: &str) -> &'a (String, NodeKind) {
    nodes
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("node `{name}` not found in {nodes:#?}"))
}

#[test]
fn test_nim_simple_alias_emits_typedef() {
    let nodes = parse("type Score = int\n");
    let n = find_node(&nodes, "Score");
    assert_eq!(n.1, NodeKind::Typedef, "simple type alias must be NodeKind::Typedef");
}

#[test]
fn test_nim_proc_alias_emits_typedef() {
    let nodes = parse("type Cb = proc(x: int): int\n");
    let n = find_node(&nodes, "Cb");
    assert_eq!(n.1, NodeKind::Typedef, "alias to proc type must be NodeKind::Typedef");
}

#[test]
fn test_nim_tuple_alias_emits_typedef() {
    let nodes = parse("type Pair = tuple[a, b: int]\n");
    let n = find_node(&nodes, "Pair");
    assert_eq!(n.1, NodeKind::Typedef, "tuple alias must be NodeKind::Typedef");
}

#[test]
fn test_nim_object_not_typedef() {
    let src = "type Person = object\n  name: string\n  age: int\n";
    let nodes = parse(src);
    // Person must be Class, not Typedef
    let n = find_node(&nodes, "Person");
    assert_eq!(n.1, NodeKind::Class, "object type must be NodeKind::Class, not Typedef");
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "object type must not emit any Typedef, got: {nodes:#?}"
    );
}

#[test]
fn test_nim_enum_not_typedef() {
    // `type Foo = enum` parses as type_declaration with an enum_declaration
    // child; the Typedef filter must reject it so it doesn't shadow the
    // Class/Enum path.
    let nodes = parse("type Color = enum\n  Red\n  Blue\n");
    assert!(
        nodes.iter().all(|(_, k)| *k != NodeKind::Typedef),
        "enum type must not emit Typedef, got: {nodes:#?}"
    );
}

#[test]
fn test_nim_multiple_aliases_in_type_section() {
    let src = "type\n  Score = int\n  Person = object\n    name: string\n";
    let nodes = parse(src);
    let score = find_node(&nodes, "Score");
    assert_eq!(score.1, NodeKind::Typedef, "Score must be Typedef");
    let person = find_node(&nodes, "Person");
    assert_eq!(person.1, NodeKind::Class, "Person must be Class");
}
