//! JavaScript-side `path_literals` extractor regression tests.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let graph = provider
        .parse_file(Path::new("test.js"), src.as_bytes())
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
const fs = require("fs");
function load() {
    return fs.readFileSync("session_meta.json", "utf-8");
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
    let src = r#"
const fs = require("fs");
function reader() {
    return fs.readFileSync("meta.json", "utf-8");
}
function writer(d) {
    fs.writeFileSync("session_meta.json", d);
}
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}

#[test]
fn test_path_literals_nested_function_context_preserves_names_and_sinks() {
    let source = r#"
class Repository {
  load() {
    const arrow = () => fs.readFileSync(`arrow.json`);
    run(function named() { fs.writeFileSync('named.json', 'data'); });
    run(function () { fs.readFileSync('anonymous.json'); });
    fs.readFileSync({ path: 'nested.json' });
    const dynamic = `skip-${name}.json`;
  }
}
function other() { fs.readFileSync('other.json'); }
"#;
    let literals = parse_path_literals(source);
    for (value, symbol, owner, reason) in [
        ("arrow.json", "arrow", Some("Repository"), "sink:read"),
        ("named.json", "named", Some("Repository"), "sink:write"),
        ("anonymous.json", "load", Some("Repository"), "sink:read"),
        ("other.json", "other", None, "sink:read"),
    ] {
        let literal = find_by_value(&literals, value);
        assert_eq!(literal.enclosing_symbol.as_deref(), Some(symbol));
        assert_eq!(literal.enclosing_owner.as_deref(), owner);
        assert!(literal.sink_reason.starts_with(reason), "{literal:?}");
    }
    let nested = find_by_value(&literals, "nested.json");
    assert!(!nested.sink_reason.starts_with("sink:read"));
    assert!(!literals
        .iter()
        .any(|literal| literal.value.contains("skip-")));
}

#[test]
fn test_path_literals_long_chain_preserves_each_literal() {
    let source = format!(
        "class Repository {{ load(fs) {{ fs{}; }} }}",
        ".readFileSync('entry.json')".repeat(200)
    );
    let literals = parse_path_literals(&source);
    assert_eq!(literals.len(), 200);
    for literal in literals {
        assert_eq!(literal.enclosing_symbol.as_deref(), Some("load"));
        assert_eq!(literal.enclosing_owner.as_deref(), Some("Repository"));
        assert!(literal.sink_reason.starts_with("sink:read"));
    }
}
