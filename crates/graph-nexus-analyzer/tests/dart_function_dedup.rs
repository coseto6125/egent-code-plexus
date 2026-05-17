//! Regression: dart/queries.scm had two patterns capturing `@function.name`
//! for every regular function — `(function_declaration (function_signature …))`
//! AND a bare `(function_signature …)`. Tree-sitter-dart emits both the outer
//! `function_declaration` and the inner `function_signature`, so both fired.
//! Round 8 baseline showed Dart Function rs=1515 vs ref=314, ~5x over-emit;
//! the parser's `dedup_by` only collapses adjacent rows with matching span,
//! and the two captures produced different spans (with-body vs signature-only),
//! so duplicates survived.

use graph_nexus_analyzer::dart::parser::DartProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn functions(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn single_top_level_function_emits_once() {
    let g = parse("void main() { print('hi'); }\n");
    let fns = functions(&g);
    assert_eq!(fns, vec!["main"], "nodes: {:?}", g.nodes);
}

#[test]
fn three_top_level_functions_each_emit_once() {
    let g = parse("void a() {}\nint b() { return 0; }\nString c() { return 's'; }\n");
    let fns = functions(&g);
    assert_eq!(fns, vec!["a", "b", "c"], "nodes: {:?}", g.nodes);
}

#[test]
fn external_top_level_emits_once() {
    let g = parse("external int fastCount();\n");
    let fns = functions(&g);
    assert_eq!(
        fns,
        vec!["fastCount"],
        "external should still emit once: {:?}",
        g.nodes
    );
}

#[test]
fn class_method_not_emitted_as_function() {
    let g = parse("class C { void m() {} }\n");
    let fns = functions(&g);
    assert!(
        fns.is_empty(),
        "class methods should NOT emit Function (they're Method): {:?}",
        g.nodes
    );
}
