//! Rust-side `path_literals` extractor regression tests. Validates the
//! side-table populated by `receiver_types::extract_rust_calls_and_path_literals`
//! before the post-process pass promotes it into PathLiteral nodes / edges.
//!
//! Bar for C3:
//!   - is_path_shaped accepts/rejects per `path_literal::is_path_shaped`.
//!   - sink_reason is the verbatim payload classify_sink/sink_reason produce.
//!   - enclosing_symbol / enclosing_owner resolve for fn / impl-method
//!     positions; None for module-level literals.
//!   - format-string FP (`"\n"`) doesn't pollute the side table.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = RustProvider::new().expect("RustProvider::new");
    let graph = provider
        .parse_file(Path::new("test.rs"), src.as_bytes())
        .expect("parse_file");
    graph
        .path_literals
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

fn find_by_value<'a>(lits: &'a [RawPathLiteral], value: &str) -> &'a RawPathLiteral {
    lits.iter()
        .find(|l| l.value == value)
        .unwrap_or_else(|| panic!("expected path literal {value:?}, got: {lits:?}"))
}

#[test]
fn free_function_with_read_sink_classified_as_read_high() {
    let src = r#"
use std::fs;
fn load() -> std::io::Result<String> {
    fs::read_to_string("session_meta.json")
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert_eq!(lit.enclosing_owner, None);
    assert_eq!(lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn impl_method_with_write_sink_resolves_owner_class() {
    let src = r#"
struct Writer;
impl Writer {
    fn dump(&self) {
        std::fs::write("output.json", b"{}").unwrap();
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("dump"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("Writer"));
    assert_eq!(lit.sink_reason, "sink:write|confidence:medium");
}

#[test]
fn module_level_const_emits_pathliteral_without_enclosing_fn() {
    let src = r#"
const META: &str = "session_meta.json";
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol, None);
    assert_eq!(lit.enclosing_owner, None);
    assert_eq!(lit.sink_reason, "sink:free|confidence:high");
}

#[test]
fn format_string_with_escape_rejected_by_predicate() {
    let src = r#"
fn shout() {
    println!("hello\nworld");
}
"#;
    let lits = parse_path_literals(src);
    let has_escape_str = lits.iter().any(|l| l.value.contains("hello"));
    assert!(
        !has_escape_str,
        "format string with \\n escape must not be path-shaped: {lits:?}"
    );
}

#[test]
fn raw_string_literal_captured_with_correct_value() {
    let src = r#"
fn cfg_path() -> &'static str {
    r"C:\Users\me\config.json"
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, r"C:\Users\me\config.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("cfg_path"));
}

#[test]
fn join_method_call_classified_as_join_medium() {
    let src = r#"
use std::path::PathBuf;
fn assemble(base: PathBuf) -> PathBuf {
    base.join("session_meta.json")
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("assemble"));
    assert_eq!(lit.sink_reason, "sink:join|confidence:medium");
}

#[test]
fn with_file_name_path_value_classified_as_ext_change_high() {
    let src = r#"
use std::path::PathBuf;
fn swap(p: PathBuf) -> PathBuf {
    p.with_file_name("session_meta.json")
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.sink_reason, "sink:ext-change|confidence:high");
}

#[test]
fn let_binding_classified_as_free_high() {
    let src = r#"
fn cfg() {
    let _x = "settings.toml";
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "settings.toml");
    assert_eq!(lit.sink_reason, "sink:free|confidence:high");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("cfg"));
}

#[test]
fn pr357_minirepro_split_brain_emits_both_names() {
    let src = r#"
use std::fs;
fn writer() {
    fs::write("session_meta.json", b"{}").unwrap();
}
fn reader() -> std::io::Result<String> {
    fs::read_to_string("meta.json")
}
"#;
    let lits = parse_path_literals(src);
    assert!(
        lits.iter().any(|l| l.value == "session_meta.json"),
        "writer-side literal missing: {lits:?}"
    );
    assert!(
        lits.iter().any(|l| l.value == "meta.json"),
        "reader-side literal missing: {lits:?}"
    );
    let writer_lit = find_by_value(&lits, "session_meta.json");
    let reader_lit = find_by_value(&lits, "meta.json");
    assert_eq!(writer_lit.sink_reason, "sink:write|confidence:medium");
    assert_eq!(reader_lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn no_url_emission_for_http_strings() {
    let src = r#"
fn fetch() {
    let _u = "https://api.example.com/v1/items.json";
}
"#;
    let lits = parse_path_literals(src);
    assert!(
        lits.is_empty() || lits.iter().all(|l| !l.value.starts_with("http")),
        "URLs must not surface as PathLiteral: {lits:?}"
    );
}
