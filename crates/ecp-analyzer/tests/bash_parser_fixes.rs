//! Bash provider fixes: function-body assignments are not graph nodes.

use ecp_analyzer::bash::parser::BashProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    BashProvider::new()
        .expect("BashProvider::new")
        .parse_file(Path::new("t.sh"), src.as_bytes())
        .expect("parse_file")
}

fn const_rows(g: &LocalGraph) -> Vec<(&str, u32)> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .map(|n| (n.name.as_str(), n.span.0))
        .collect()
}

/// `local` declarations and plain assignments inside a function body are
/// locals: only the top-level `TOP` is a Const, and the function stays.
#[test]
fn test_bash_function_body_assignments_are_not_const_nodes() {
    let g = parse(
        "TOP=1\ngenerate_cert() { local name=$1; local cn=\"$2\"; count=5; echo $name; }\nfunction other {\n  local z=1\n}\n",
    );
    assert_eq!(const_rows(&g), [("TOP", 0)]);
    let functions: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(functions, ["generate_cert", "other"]);
}

/// A name assigned inside a function first and at top level later is the
/// top-level Const, at its own line (the local must not claim the name).
#[test]
fn test_bash_function_body_assignment_does_not_shadow_later_top_level_const() {
    let g = parse("f() { X=2; }\nX=1\n");
    assert_eq!(const_rows(&g), [("X", 1)]);
}

/// A top-level `export` / `declare` is still a Const; a function's `local`
/// of the same name is not a second one.
#[test]
fn test_bash_top_level_declaration_command_is_still_const() {
    let g = parse("export PATH_X=/a\nf() { local PATH_X=b; }\n");
    assert_eq!(const_rows(&g), [("PATH_X", 0)]);
}

#[test]
fn test_bash_empty_and_assignment_free_function_emit_no_const() {
    assert!(const_rows(&parse("")).is_empty());
    assert!(const_rows(&parse("f() { echo hi; }\n")).is_empty());
}

// Contract: an assignment is a Const exactly when no `function_definition`
// contains it, wherever the function sits: before, between and after the
// top-level assignments, nested in another function, or under an `if`.
#[test]
fn test_bash_const_outside_every_function_shape_is_kept_and_inside_is_dropped() {
    let g = parse(
        "A=1\nf() { B=2; g() { C=3; }; D=4; }\nE=5\nif true; then\n  h() { F=6; }\n  G=7\nfi\nfunction k {\n  H=8\n}\nI=9\n",
    );
    let names: Vec<&str> = const_rows(&g).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["A", "E", "G", "I"]);
}

// Contract: thousands of top-level assignments with a function in the
// middle all stay Consts; the function body's own assignment does not.
#[test]
fn test_bash_many_top_level_assignments_around_a_function_all_stay_const() {
    let mut src: String = (0..3000).map(|i| format!("V{i}=1\n")).collect();
    src.push_str("f() { INNER=1; }\n");
    src.extend((3000..6000).map(|i| format!("V{i}=1\n")));
    let g = parse(&src);
    let rows = const_rows(&g);
    assert_eq!(rows.len(), 6000);
    assert!(rows.iter().all(|(name, _)| *name != "INNER"));
}
