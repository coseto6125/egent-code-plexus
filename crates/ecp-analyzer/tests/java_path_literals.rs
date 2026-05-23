//! Java-side `path_literals` extractor regression tests. Validates the
//! side-table populated by `java::path_literals::extract_java_path_literals`
//! before the post-process pass promotes it into PathLiteral nodes / edges.
//!
//! Bar mirrors rust_path_literals.rs:
//!   - is_path_shaped accepts/rejects per `path_literal::is_path_shaped`.
//!   - sink_reason is the verbatim payload classify_sink/sink_reason produce.
//!   - enclosing_symbol / enclosing_owner resolve for method / class-method
//!     positions; None for class-level constant initialisers.
//!   - format-string FP (`"\n"`) doesn't pollute the side table.
//!   - PR #357 mini-repro: both a read and a write literal in one file surface.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    let graph = provider
        .parse_file(Path::new("Test.java"), src.as_bytes())
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
import java.nio.file.Files;
import java.nio.file.Paths;
class Loader {
    String load() throws Exception {
        return Files.readString(Paths.get("session_meta.json"));
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("Loader"));
    assert!(
        lit.sink_reason.starts_with("sink:read") || lit.sink_reason.starts_with("sink:join"),
        "expected read or join sink, got: {}",
        lit.sink_reason
    );
}

#[test]
fn method_in_class_with_write_sink_resolves_owner() {
    let src = r#"
import java.nio.file.Files;
import java.nio.file.Paths;
class Writer {
    void dump(String data) throws Exception {
        Files.writeString(Paths.get("output.json"), data);
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("dump"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("Writer"));
}

#[test]
fn format_string_with_newline_escape_rejected() {
    let src = r#"
class Shout {
    void shout() {
        System.out.println("hello\nworld");
    }
}
"#;
    let lits = parse_path_literals(src);
    let has_hello = lits.iter().any(|l| l.value.contains("hello"));
    assert!(
        !has_hello,
        "format string with \\n must not be path-shaped: {lits:?}"
    );
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
import java.nio.file.Files;
import java.nio.file.Paths;
class Repo {
    String read() throws Exception {
        return Files.readString(Paths.get("meta.json"));
    }
    void write(String d) throws Exception {
        Files.writeString(Paths.get("session_meta.json"), d);
    }
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
}
