//! Kotlin-side `path_literals` extractor regression tests. Validates the
//! side-table populated by `kotlin::path_literals::extract_kotlin_path_literals`
//! before the post-process pass promotes it into PathLiteral nodes / edges.
//!
//! Bar mirrors rust_path_literals.rs:
//!   - is_path_shaped accepts/rejects.
//!   - sink_reason is the verbatim payload from classify_sink/sink_reason.
//!   - enclosing_symbol / enclosing_owner resolve for fun / class-method.
//!   - interpolated strings are NOT emitted (dynamic value = noise).
//!   - PR #357 mini-repro: both read and write literals in one file surface.

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = KotlinProvider::new().expect("KotlinProvider::new");
    let graph = provider
        .parse_file(Path::new("Test.kt"), src.as_bytes())
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
import java.io.File
fun load(): String {
    return File("session_meta.json").readText()
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
    assert_eq!(lit.enclosing_owner, None);
    // FU-2026-05-23-023: `File("x").readText()` chain promotes the sink
    // from the constructor (`File` → join|medium) to the terminal method
    // (`readText` → read|high).
    assert_eq!(lit.sink_reason, "sink:read|confidence:high");
}

#[test]
fn method_in_class_with_write_sink_resolves_owner() {
    let src = r#"
import java.io.File
class FileWriter {
    fun dump(data: String) {
        File("output.json").writeText(data)
    }
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "output.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("dump"));
    assert_eq!(lit.enclosing_owner.as_deref(), Some("FileWriter"));
    // FU-2026-05-23-023: same promotion for write chain.
    assert_eq!(lit.sink_reason, "sink:write|confidence:high");
}

#[test]
fn interpolated_string_not_emitted() {
    let src = r#"
fun greet(name: String) {
    val msg = "config_${name}.json"
    println(msg)
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
import java.io.File
fun reader(): String = File("meta.json").readText()
fun writer(d: String) { File("session_meta.json").writeText(d) }
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
    // Same flow-analysis caveat as the read-sink test above — the immediate
    // sink is the `File()` constructor, so per the call-context-only rule
    // both classify as non-free (Join / something path-ish) rather than the
    // chained `.readText()` / `.writeText()` semantic. The assertion guards
    // the fact that they DO surface; precise sink classification is P2.
}
