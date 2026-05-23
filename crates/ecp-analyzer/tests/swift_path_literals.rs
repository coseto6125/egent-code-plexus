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

// FU-2026-05-23-023 — Swift idiom analysis.
//
// Swift path I/O falls into two shapes:
//   1. Labelled-argument constructor: `String(contentsOfFile: "x")`,
//      `Data(contentsOf: url)`. The argument *label* (contentsOfFile / toFile)
//      carries the read/write semantics; the type name alone (`String`) would
//      resolve to Free. enclosing_callee now promotes the label over the
//      bare-identifier function name → read|confidence:high.
//   2. Navigation chain: `"str".write(toFile: "x", ...)`. The callee is
//      already a navigation_expression; trailing_ident extracts `write`
//      (Medium Write). No chain-promotion walk needed — Swift chains put the
//      path in a labelled argument of the chained method, not in the receiver.

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
    // enclosing_callee promotes the argument label `contentsOfFile`;
    // classify_sink classifies that as Read|High.
    assert_eq!(lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn labelled_arg_constructor_contentsoffile() {
    // `String(contentsOfFile: "config.json")` — argument label is promoted
    // over the bare type name so classify_sink sees `contentsOfFile`, not
    // `String`.
    let src = r#"let data = try String(contentsOfFile: "config.json")"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "config.json");
    assert_eq!(lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn navigation_chain_write_tofile() {
    // `"str".write(toFile: "output.json", ...)` — Swift chains put the path
    // in a labelled arg of the chained method, not the receiver. The
    // `toFile:` label promotes over the trailing `write`, classifying as
    // Write|High.
    let src = r#"try "hello".write(toFile: "output.json", atomically: true, encoding: .utf8)"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(lit.sink_reason, "sink:write|confidence:high");
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
