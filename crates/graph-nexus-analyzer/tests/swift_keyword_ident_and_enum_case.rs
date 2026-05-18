//! Two Swift parser gaps surfaced in Round 67:
//!
//! 1. **Keyword-as-identifier** — Swift 5.9+ context keywords like
//!    `package`, `actor`, `await` reuse identifier slots. tree-sitter-swift
//!    represents that with `simple_identifier > <keyword-token>` (the
//!    `simple_identifier` is no longer a leaf). The previous
//!    `collect_pattern_names_rec` filtered on `child_count() == 0` and
//!    dropped them silently. Repro: `let package = Package(...)` in every
//!    real `Package.swift` produced **zero** nodes.
//!
//! 2. **Enum cases** — `case foo` / `case a, b, c` were not captured at all
//!    (no query in queries.scm). ref-gitnexus emits each case name as
//!    Property; gnx-rs missed every one.

use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("SwiftProvider init");
    p.parse_file(Path::new("t.swift"), src.as_bytes())
        .expect("parse_file")
}

fn names_of(g: &LocalGraph, kind: NodeKind) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

// ── Keyword-as-identifier ───────────────────────────────────────────────────

#[test]
fn module_level_let_package_emits_variable() {
    // Real Package.swift pattern — `let package = Package(...)`.
    let g = parse("let package = Package(name: \"X\")\n");
    let vars = names_of(&g, NodeKind::Variable);
    assert!(
        vars.contains(&"package"),
        "expected `package` in Variable nodes: {vars:?}"
    );
}

#[test]
fn module_level_let_actor_emits_variable() {
    // Same path with another Swift 5.5+ context keyword.
    let g = parse("let actor = SomeActor()\n");
    let vars = names_of(&g, NodeKind::Variable);
    assert!(
        vars.contains(&"actor"),
        "expected `actor` in Variable nodes: {vars:?}"
    );
}

#[test]
fn plain_module_level_let_still_works() {
    // Regression guard: the keyword-as-identifier fix must not break the
    // common case where `simple_identifier` is a bare leaf.
    let g = parse("let x = 5\nlet y = \"hello\"\n");
    let vars = names_of(&g, NodeKind::Variable);
    assert!(vars.contains(&"x"));
    assert!(vars.contains(&"y"));
}

#[test]
fn tuple_let_destructure_still_works() {
    // `let (a, b) = ...` produces two distinct simple_identifier leaves
    // under the pattern; both must still be picked up.
    let g = parse("let (a, b) = (1, 2)\n");
    let vars = names_of(&g, NodeKind::Variable);
    assert!(vars.contains(&"a"));
    assert!(vars.contains(&"b"));
}

// ── Enum cases ──────────────────────────────────────────────────────────────

#[test]
fn enum_case_simple_emits_property() {
    let g = parse("enum E {\n    case foo\n    case bar\n}\n");
    let props = names_of(&g, NodeKind::Property);
    assert!(props.contains(&"foo"), "expected foo in {props:?}");
    assert!(props.contains(&"bar"), "expected bar in {props:?}");
}

#[test]
fn enum_case_with_associated_values_emits_property() {
    // Alamofire AFError-style associated-value cases.
    let g = parse(
        "enum AFError {\n    case bodyPartFileIsDirectory(at: URL)\n    case bodyPartFileNotReachable(at: URL)\n}\n",
    );
    let props = names_of(&g, NodeKind::Property);
    assert!(props.contains(&"bodyPartFileIsDirectory"), "{props:?}");
    assert!(props.contains(&"bodyPartFileNotReachable"), "{props:?}");
}

#[test]
fn enum_case_multi_name_emits_one_property_per_name() {
    // `case a, b, c` packs three simple_identifier children into one
    // enum_entry; emit three Property nodes.
    let g = parse("enum E { case a, b, c }\n");
    let props = names_of(&g, NodeKind::Property);
    assert!(props.contains(&"a"));
    assert!(props.contains(&"b"));
    assert!(props.contains(&"c"));
}

#[test]
fn enum_without_cases_emits_no_property() {
    // Regression guard.
    let g = parse("enum E {\n    func foo() {}\n}\n");
    let props = names_of(&g, NodeKind::Property);
    assert!(
        props.is_empty(),
        "enum methods must not leak as Property; got {props:?}"
    );
}
