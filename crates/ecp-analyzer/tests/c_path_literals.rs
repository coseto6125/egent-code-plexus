//! C-side `path_literals` extractor regression tests.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = CProvider::new().expect("CProvider::new");
    let graph = provider
        .parse_file(Path::new("test.c"), src.as_bytes())
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
fn function_with_open_sink() {
    let src = r#"
#include <stdio.h>

FILE *load(void) {
    return fopen("session_meta.json", "r");
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert!(
        lit.sink_reason.starts_with("sink:open"),
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
#include <stdio.h>

FILE *reader(void) {
    return fopen("meta.json", "r");
}
FILE *writer(void) {
    return fopen("session_meta.json", "w");
}
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}

#[test]
fn concatenated_string_joined() {
    let src = r#"
#include <stdio.h>

FILE *load(void) {
    return fopen("sess" "ion_meta.json", "r");
}
"#;
    let lits = parse_path_literals(src);
    assert!(
        lits.iter().any(|l| l.value == "session_meta.json"),
        "concatenated string not joined: {lits:?}"
    );
}
