//! PHP-side `path_literals` extractor regression tests.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = PhpProvider::new().expect("PhpProvider::new");
    let graph = provider
        .parse_file(Path::new("test.php"), src.as_bytes())
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
    let src = r#"<?php
function load() {
    return file_get_contents("session_meta.json");
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert!(
        lit.sink_reason.starts_with("sink:read"),
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"<?php
function reader() {
    return file_get_contents("meta.json");
}
function writer($d) {
    file_put_contents("session_meta.json", $d);
}
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}
