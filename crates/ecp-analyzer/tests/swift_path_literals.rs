//! Swift-side `path_literals` extractor regression tests.

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = SwiftProvider::new().expect("SwiftProvider::new");
    let graph = provider
        .parse_file(Path::new("test.swift"), src.as_bytes())
        .expect("parse_file");
    graph
        .path_literals
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

fn find_by_value<'a>(lits: &'a [RawPathLiteral], value: &str) -> &'a RawPathLiteral {
    lits.iter()
        .find(|l| l.value == value)
        .unwrap_or_else(|| panic!("expected literal {value:?}, got: {lits:?}"))
}

#[test]
fn function_with_read_sink() {
    let src = r#"
import Foundation

func load() throws -> String {
    return try String(contentsOfFile: "session_meta.json")
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
import Foundation

func reader() throws -> String {
    return try String(contentsOfFile: "meta.json")
}
func writer(_ d: String) throws {
    try d.write(toFile: "session_meta.json", atomically: true, encoding: .utf8)
}
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}
