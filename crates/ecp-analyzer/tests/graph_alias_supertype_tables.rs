//! The graph archive keeps the path aliases the index resolved with and each
//! type's resolved supertypes, so the session overlay reads the same facts a
//! full reindex used (`ZeroCopyGraph::alias_table` / `supertype_table`).

use ecp_analyzer::python::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::ArchivedZeroCopyGraph;
use std::path::Path;

const HERITAGE: &str = "\
class Base:
    pass


class Child(Base):
    pass


class Mixed(Base, External):
    pass


class Lead(External, Base):
    pass


class Orphan(External):
    pass


class Plain:
    pass
";

fn archive(builder: GraphBuilder) -> rkyv::util::AlignedVec {
    rkyv::to_bytes::<rkyv::rancor::Error>(&builder.build()).unwrap()
}

fn access(bytes: &[u8]) -> &ArchivedZeroCopyGraph {
    rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap()
}

fn only_node(graph: &ArchivedZeroCopyGraph, name: &str) -> u32 {
    let mut hits = graph.nodes_by_name(name);
    let idx = hits.next().unwrap_or_else(|| panic!("no node {name}"));
    assert!(hits.next().is_none(), "{name} is not unique");
    idx
}

#[test]
fn test_build_supertypes_archived_as_node_indices_in_declared_order() {
    let provider = PythonProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        provider
            .parse_file(Path::new("pkg/m.py"), HERITAGE.as_bytes())
            .unwrap(),
    );
    let bytes = archive(builder);
    let graph = access(&bytes);

    let base = only_node(graph, "Base");
    assert_eq!(
        graph.supertypes_of(only_node(graph, "Child")),
        Some((vec![base], None)),
        "every base resolved"
    );
    assert_eq!(
        graph.supertypes_of(only_node(graph, "Mixed")),
        Some((vec![base], Some(1))),
        "the external base comes after one resolved base"
    );
    assert_eq!(
        graph.supertypes_of(only_node(graph, "Lead")),
        Some((vec![base], Some(0))),
        "an external base listed first stays ahead of the project base"
    );
    assert_eq!(
        graph.supertypes_of(only_node(graph, "Orphan")),
        Some((vec![], Some(0))),
        "every declared base unresolved"
    );
    assert_eq!(
        graph.supertypes_of(only_node(graph, "Plain")),
        None,
        "a type with no heritage has no entry"
    );
    assert_eq!(graph.supertypes_of(base), None);
}

#[test]
fn test_build_without_aliases_archives_an_empty_alias_table() {
    let provider = TypeScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        provider
            .parse_file(Path::new("src/a.ts"), b"export function f() {}\n")
            .unwrap(),
    );
    let bytes = archive(builder);
    let graph = access(&bytes);

    assert!(graph.path_aliases().is_empty());
    assert_eq!(graph.supertypes_of(0), None, "no heritage, no entry");
}

#[test]
fn test_build_path_aliases_archived_in_declaration_order_with_duplicates() {
    let provider = TypeScriptProvider::new().unwrap();
    let mut aliases = PathAliases::new();
    aliases.add("@/*", vec!["src/*".to_string()]);
    aliases.add("config", vec!["app/config.ts".to_string()]);
    aliases.add("@/*", vec!["lib/*".to_string(), "vendor/*".to_string()]);
    let mut builder = GraphBuilder::new().with_path_aliases(aliases);
    builder.add_graph(
        provider
            .parse_file(Path::new("src/a.ts"), b"export function f() {}\n")
            .unwrap(),
    );
    let bytes = archive(builder);
    let graph = access(&bytes);

    let archived = graph.path_aliases();
    let got: Vec<(&str, Vec<String>)> = archived
        .entries()
        .map(|(pattern, replacements)| (pattern, replacements.to_vec()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("@/*", vec!["src/*".to_string()]),
            ("config", vec!["app/config.ts".to_string()]),
            ("@/*", vec!["lib/*".to_string(), "vendor/*".to_string()]),
        ]
    );
}
