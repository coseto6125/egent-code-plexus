//! C++-side `path_literals` extractor regression tests.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn parse_path_literals(src: &str) -> Vec<RawPathLiteral> {
    let provider = CppProvider::new().expect("CppProvider::new");
    let graph = provider
        .parse_file(Path::new("test.cpp"), src.as_bytes())
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
#include <fstream>

std::ifstream load() {
    return std::ifstream("session_meta.json");
}
"#;
    let lits = parse_path_literals(src);
    let lit = find_by_value(&lits, "session_meta.json");
    assert_eq!(lit.enclosing_symbol.as_deref(), Some("load"));
}

#[test]
fn pr357_minirepro_both_literals_surface() {
    let src = r#"
#include <fstream>

std::ifstream reader() { return std::ifstream("meta.json"); }
std::ofstream writer() { return std::ofstream("session_meta.json"); }
"#;
    let lits = parse_path_literals(src);
    assert!(lits.iter().any(|l| l.value == "meta.json"));
    assert!(lits.iter().any(|l| l.value == "session_meta.json"));
}

#[test]
fn filesystem_path_chain_replace_extension_promotes_to_ext_change_high() {
    // FU-2026-05-23-023: `std::filesystem::path("x.json").replace_extension("toml")`
    // chains the inner constructor (`path`) into the terminal
    // `replace_extension` op. Both literals must surface:
    //   - "config.json" is path-shaped + chain promotes to ext-change|high
    //   - "toml" is short non-path-shaped, but the FU-024 sink-override
    //     (is_ext_change_callee) accepts it because the call is an ext-change op
    let src = r#"
#include <filesystem>
void rename() {
    auto p = std::filesystem::path("config.json").replace_extension("toml");
}
"#;
    let lits = parse_path_literals(src);
    let primary = find_by_value(&lits, "config.json");
    assert_eq!(
        primary.sink_reason, "sink:ext-change|confidence:high",
        "primary path literal must promote via chain; got: {}",
        primary.sink_reason
    );
    let ext = find_by_value(&lits, "toml");
    assert_eq!(
        ext.sink_reason, "sink:ext-change|confidence:high",
        "short ext literal must surface via FU-024 sink-override; got: {}",
        ext.sink_reason
    );
}
