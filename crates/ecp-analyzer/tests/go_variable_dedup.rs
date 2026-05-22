//! Regression tests for Go top-level `var` declaration de-duplication.
//!
//! The Go queries have two overlapping patterns:
//!   `@var`      — `(var_spec name: ... type: ...)` — any depth, typed vars only
//!   `@variable` — `(source_file (var_declaration (var_spec name: ...)))` — file-scope
//!
//! For a top-level typed var like `var Validator StructValidator = &defaultValidator{}`
//! BOTH patterns fire. The old `file_var_pending` path used `source_file.end_position()`
//! as the span end, which never matched the `@var` path's `var_spec.end_position()`.
//! The dedup check `name == pending.name && span == pending.span` therefore failed,
//! and the same var was emitted twice → uid collision BlindSpot.
//!
//! After the fix the `@variable` path uses the `var_spec` parent for span,
//! so the spans agree and the duplicate is suppressed.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse_variables(src: &str) -> Vec<String> {
    let provider = GoProvider::new().expect("GoProvider init");
    let graph = provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file");
    graph
        .nodes
        .into_iter()
        .filter(|n| n.kind == NodeKind::Variable)
        .map(|n| n.name.clone())
        .collect()
}

#[test]
fn typed_file_scope_var_emitted_once() {
    // Mirrors `binding.go:72`: `var Validator StructValidator = &defaultValidator{}`
    // Both `@var` (has explicit type) and `@variable` (top-level) match this node.
    // Only one Variable should be emitted.
    let src = "package p\ntype T interface{}\nvar Validator T = nil\n";
    let vars = parse_variables(src);
    let count = vars.iter().filter(|n| n.as_str() == "Validator").count();
    assert_eq!(
        count, 1,
        "expected exactly one Variable(Validator), got {count}: {vars:#?}"
    );
}

#[test]
fn untyped_file_scope_var_emitted_once() {
    // `var X = 42` — only `@variable` fires (no explicit type → `@var` skips).
    let src = "package p\nvar X = 42\n";
    let vars = parse_variables(src);
    let count = vars.iter().filter(|n| n.as_str() == "X").count();
    assert_eq!(
        count, 1,
        "expected one Variable(X) for untyped var, got {count}"
    );
}

#[test]
fn typed_var_block_each_emitted_once() {
    // `var ( A int = 1; B string = "x" )` — both typed, both should appear exactly once.
    let src = "package p\nvar (\n    A int = 1\n    B string = \"x\"\n)\n";
    let vars = parse_variables(src);
    let a_count = vars.iter().filter(|n| n.as_str() == "A").count();
    let b_count = vars.iter().filter(|n| n.as_str() == "B").count();
    assert_eq!(a_count, 1, "A should appear once, got {a_count}: {vars:#?}");
    assert_eq!(b_count, 1, "B should appear once, got {b_count}: {vars:#?}");
}

#[test]
fn multiple_top_level_typed_vars_no_duplicates() {
    // Larger case: several typed top-level vars — each emitted exactly once.
    let src = r#"package p
var MIMEJSON string = "application/json"
var MIMEHTML string = "text/html"
var MIMEMultipartPOSTForm string = "multipart/form-data"
"#;
    let vars = parse_variables(src);
    for name in &["MIMEJSON", "MIMEHTML", "MIMEMultipartPOSTForm"] {
        let count = vars.iter().filter(|n| n.as_str() == *name).count();
        assert_eq!(
            count, 1,
            "{name} should appear exactly once, got {count}: {vars:#?}"
        );
    }
}
