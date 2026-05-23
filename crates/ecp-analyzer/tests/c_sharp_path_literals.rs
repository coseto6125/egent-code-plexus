//! C#-side `path_literals` extractor regression tests. Validates the
//! side-table populated by `c_sharp::path_literals::extract_csharp_path_literals`
//! before the post-process pass promotes it into PathLiteral nodes / edges.
//!
//! Bar mirrors rust_path_literals.rs:
//!   - is_path_shaped accepts/rejects.
//!   - sink_reason is the verbatim payload from classify_sink/sink_reason.
//!   - enclosing_symbol / enclosing_owner resolve for method / class-method.
//!   - interpolated strings (`$"..."`) are NOT emitted.
//!   - PR #357 mini-repro: both read and write literals in one file surface.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    let graph = provider
        .parse_file(Path::new("Test.cs"), src.as_bytes())
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
fn free_method_with_read_sink_classified_as_read_high() {
    let src = r#"
using System.IO;
class Loader {
    string Load() {
        return File.ReadAllText("session_meta.json");
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("Load"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("Loader"));
    assert_eq!(lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn method_in_class_with_write_sink_resolves_owner() {
    let src = r#"
using System.IO;
class FileWriter {
    void Dump(string data) {
        File.WriteAllText("output.json", data);
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("Dump"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("FileWriter"));
    assert_eq!(lit.sink_reason, "sink:write|confidence:high");
}

#[test]
fn interpolated_string_not_emitted() {
    let src = r#"
using System.IO;
class Formatter {
    void Process(string name) {
        var path = $"config_{name}.json";
        File.ReadAllText(path);
    }
}
"#;
    let lits = parse_path_literals(src);
    let has_interpolated = lits.iter().any(|l| l.value.contains("config_"));
    assert!(
        !has_interpolated,
        "interpolated string must not surface as PathLiteral: {lits:?}"
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
using System.IO;
class Repo {
    string Read() => File.ReadAllText("meta.json");
    void Write(string d) { File.WriteAllText("session_meta.json", d); }
}
"#;
    let lits = parse_path_literals(src);
    assert!(
        lits.iter().any(|l| l.value == "meta.json"),
        "reader-side literal missing: {lits:?}"
    );
    assert!(
        lits.iter().any(|l| l.value == "session_meta.json"),
        "writer-side literal missing: {lits:?}"
    );
    let reader_lit = find_by_value(&lits, "meta.json");
    let writer_lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(reader_lit.sink_reason, "sink:read|confidence:high");
    assert_eq!(writer_lit.sink_reason, "sink:write|confidence:high");
}
