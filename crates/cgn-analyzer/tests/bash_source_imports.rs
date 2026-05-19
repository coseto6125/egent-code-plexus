//! Integration tests for Bash `source` / `.` (dot-source) import detection.
//!
//! Validates that the BashProvider emits `RawImport` entries for module
//! composition statements while ignoring lookalikes (variable assignments,
//! file references inside other commands).

use graph_nexus_analyzer::bash::parser::BashProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawImport;
use std::path::Path;

fn parse(source: &str) -> Vec<RawImport> {
    let provider = BashProvider::new().expect("bash provider construction");
    let graph = provider
        .parse_file(Path::new("script.sh"), source.as_bytes())
        .expect("parse bash source");
    graph.imports
}

#[test]
fn test_plain_source_emits_raw_import_with_unquoted_path() {
    let imports = parse("source lib.sh\n");
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    let imp = &imports[0];
    assert_eq!(imp.source, "lib.sh");
    assert_eq!(imp.imported_name, "*");
    assert_eq!(imp.alias, None);
}

#[test]
fn test_dot_source_emits_raw_import() {
    let imports = parse(". lib.sh\n");
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    assert_eq!(imports[0].source, "lib.sh");
}

#[test]
fn test_double_quoted_source_strips_quotes() {
    let imports = parse("source \"lib.sh\"\n");
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    assert_eq!(imports[0].source, "lib.sh");
}

#[test]
fn test_single_quoted_source_strips_quotes() {
    let imports = parse("source 'lib.sh'\n");
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    assert_eq!(imports[0].source, "lib.sh");
}

#[test]
fn test_source_with_relative_path_preserves_path() {
    let imports = parse("source ./utils/helpers.sh\n");
    assert_eq!(imports.len(), 1, "expected one import, got {imports:?}");
    assert_eq!(imports[0].source, "./utils/helpers.sh");
}

#[test]
fn test_multiple_source_statements_emit_distinct_imports() {
    let script = "source ./a.sh\n. ./b.sh\nsource \"./c.sh\"\n";
    let imports = parse(script);
    assert_eq!(imports.len(), 3, "expected three imports, got {imports:?}");
    let sources: Vec<&str> = imports.iter().map(|i| i.source.as_str()).collect();
    assert!(sources.contains(&"./a.sh"));
    assert!(sources.contains(&"./b.sh"));
    assert!(sources.contains(&"./c.sh"));
}

#[test]
fn test_non_import_command_with_source_substring_is_ignored() {
    // `echo source.txt` is not a `source` invocation — command name is `echo`.
    let imports = parse("echo source.txt\n");
    assert!(imports.is_empty(), "expected no imports, got {imports:?}");
}

#[test]
fn test_variable_assignment_named_source_is_ignored() {
    // `source=value` is a variable_assignment, not a command — must not emit.
    let imports = parse("source=value\n");
    assert!(imports.is_empty(), "expected no imports, got {imports:?}");
}
