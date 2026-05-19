//! Swift `typealias` should emit BOTH a Typedef RawNode (so graph queries by
//! NodeKind find it) AND a RawImport alias (so the named-binding pipeline
//! still resolves the alias). Previously only the Import side was emitted,
//! so `MATCH (n:Typedef) WHERE n.name = '...'` returned nothing for Swift,
//! while ref-gitnexus had 28 unpaired TypeAlias rows on Alamofire alone.

use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::{LocalGraph, RawImport, RawNode};
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = SwiftProvider::new().expect("SwiftProvider init");
    provider
        .parse_file(Path::new("t.swift"), src.as_bytes())
        .expect("parse_file")
}

fn typedefs(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes.iter().filter(|n| n.kind == NodeKind::Typedef).collect()
}

fn find_alias<'a>(imports: &'a [RawImport], alias: &str) -> &'a RawImport {
    imports
        .iter()
        .find(|i| i.alias.as_deref() == Some(alias))
        .unwrap_or_else(|| panic!("missing alias `{alias}` in {imports:#?}"))
}

#[test]
fn top_level_typealias_emits_typedef_node() {
    let g = parse("typealias MyInt = Int\n");
    let tds = typedefs(&g);
    assert_eq!(tds.len(), 1, "expected one Typedef, got {:?}", g.nodes);
    assert_eq!(tds[0].name, "MyInt");
}

#[test]
fn typealias_still_emits_import_alias() {
    // Existing behaviour preserved — alias-resolution path must keep working.
    let g = parse("typealias MyInt = Int\n");
    let alias = find_alias(&g.imports, "MyInt");
    assert_eq!(alias.imported_name, "MyInt");
    assert_eq!(alias.source, "Int");
}

#[test]
fn generic_typealias_emits_typedef_node() {
    let g = parse("public typealias AFResult<Success> = Result<Success, AFError>\n");
    let tds = typedefs(&g);
    assert_eq!(tds.len(), 1);
    assert_eq!(tds[0].name, "AFResult");
}

#[test]
fn nested_typealias_inside_struct_emits_typedef_node() {
    let g = parse(
        "public struct AsyncDataStreamSequence {\n\
             public typealias AsyncIterator = Iterator\n\
             public typealias BufferingPolicy = AsyncStream<Element>.Continuation.BufferingPolicy\n\
         }\n",
    );
    let names: Vec<&str> = typedefs(&g).iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"AsyncIterator") && names.contains(&"BufferingPolicy"),
        "expected both nested typealiases, got {:?}",
        names
    );
}

#[test]
fn multiple_top_level_typealiases_each_emit() {
    let g = parse(
        "public typealias AFDataResponse<Success> = DataResponse<Success, AFError>\n\
         public typealias AFDownloadResponse<Success> = DownloadResponse<Success, AFError>\n",
    );
    let names: Vec<&str> = typedefs(&g).iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"AFDataResponse"));
    assert!(names.contains(&"AFDownloadResponse"));
}
