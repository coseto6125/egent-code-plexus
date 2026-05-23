//! Python-side `path_literals` extractor regression tests.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    let graph = provider
        .parse_file(Path::new("test.py"), src.as_bytes())
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
fn function_with_read_sink_classified() {
    let src = r#"
def load():
    with open("session_meta.json") as f:
        return f.read()
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert!(
        lit.sink_reason.starts_with("sink:open") || lit.sink_reason.starts_with("sink:read"),
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn format_string_with_escape_rejected() {
    let src = r#"
def shout():
    print("hello\nworld")
"#;
    let lits = parse_path_literals(src);
    assert!(
        !lits.iter().any(|l| l.value.contains("hello")),
        "format string must not be path-shaped: {lits:?}"
    );
}

#[test]
fn pathlib_chain_read_text_classified_high() {
    let src = r#"
def load():
    return Path("config.json").read_text()
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "config.json");
    assert_eq!(
        lit.sink_reason, "sink:read|confidence:high",
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn pathlib_chain_write_text_classified_high() {
    let src = r#"
def save(data):
    Path("output.json").write_text(data)
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(
        lit.sink_reason, "sink:write|confidence:high",
        "got: {}",
        lit.sink_reason
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
def reader():
    with open("meta.json") as f:
        return f.read()

def writer(data):
    with open("session_meta.json", "w") as f:
        f.write(data)
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}
