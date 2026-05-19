//! Swift Named binding dimension — `typealias` and `@objc(extName)` rename.
//!
//! Mirrors Java static-import alias convention (see
//! `crates/graph-nexus-analyzer/src/java/parser.rs:189-296`): each named
//! binding emits a `RawImport` with `alias = Some(<bound name>)` so it lands
//! in the same downstream named-binding pipeline.

use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawImport;
use std::path::Path;

fn parse(src: &str) -> Vec<RawImport> {
    let provider = SwiftProvider::new().expect("SwiftProvider init");
    let graph = provider
        .parse_file(Path::new("t.swift"), src.as_bytes())
        .expect("parse_file");
    graph.imports
}

fn find_alias<'a>(imports: &'a [RawImport], alias: &str) -> &'a RawImport {
    imports
        .iter()
        .find(|i| i.alias.as_deref() == Some(alias))
        .unwrap_or_else(|| panic!("missing alias `{alias}` in {imports:#?}"))
}

#[test]
fn test_swift_typealias_emits_alias() {
    let imports = parse("typealias MyInt = Int\n");
    let r = find_alias(&imports, "MyInt");
    assert_eq!(r.imported_name, "MyInt");
    assert_eq!(r.source, "Int");
}

#[test]
fn test_swift_typealias_qualified_rhs_emits_alias() {
    let imports = parse("typealias Handler = Foundation.NSError\n");
    let r = find_alias(&imports, "Handler");
    assert_eq!(r.imported_name, "Handler");
    assert_eq!(r.source, "Foundation.NSError");
}

#[test]
fn test_swift_generic_typealias_preserves_type_parameters() {
    let imports = parse("typealias R<T> = Swift.Result<T, Error>\n");
    let r = find_alias(&imports, "R");
    assert_eq!(r.imported_name, "R");
    assert_eq!(r.source, "Swift.Result<T, Error>");
}

#[test]
fn test_swift_objc_rename_emits_alias() {
    let src = "class C {\n    @objc(extName) func intName() {}\n}\n";
    let imports = parse(src);
    let r = find_alias(&imports, "extName");
    assert_eq!(r.imported_name, "extName");
    assert_eq!(r.source, "intName");
}

#[test]
fn test_swift_objc_without_paren_emits_no_alias() {
    // Plain `@objc` exposes the symbol under its Swift name — no rename to track.
    let src = "class C {\n    @objc func ping() {}\n}\n";
    let imports = parse(src);
    assert!(
        imports.iter().all(|i| i.alias.is_none()),
        "expected no aliases, got {imports:#?}"
    );
}
