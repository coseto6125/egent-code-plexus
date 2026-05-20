//! Variable over-emission guard: only file-scope / namespace-scope globals
//! should be emitted as `NodeKind::Variable`. Function-local declarations,
//! parameters, for-loop locals, and catch parameters must be suppressed.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().expect("CppProvider init");
    let graph = provider
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

fn absent(nodes: &[RawNode], name: &str, kind: NodeKind) {
    assert!(
        nodes
            .iter()
            .find(|n| n.name == name && n.kind == kind)
            .is_none(),
        "unexpected {kind:?} `{name}` found in {nodes:#?}"
    );
}

// ── Global survives ──────────────────────────────────────────────────────────

#[test]
fn global_variable_emitted() {
    let nodes = parse("int g_count = 0;\n");
    find(&nodes, "g_count", NodeKind::Variable);
}

// ── Function-local declarations suppressed ───────────────────────────────────

#[test]
fn local_variable_not_emitted() {
    let nodes = parse("void f() { int local = 5; }\n");
    absent(&nodes, "local", NodeKind::Variable);
}

#[test]
fn function_parameter_not_emitted() {
    let nodes = parse("void f(int param) {}\n");
    absent(&nodes, "param", NodeKind::Variable);
}

#[test]
fn for_loop_local_not_emitted() {
    let nodes = parse("void f() { for (int i = 0; i < 10; ++i) {} }\n");
    absent(&nodes, "i", NodeKind::Variable);
}

#[test]
fn catch_parameter_not_emitted() {
    let nodes = parse("void f() { try {} catch (int e) {} }\n");
    absent(&nodes, "e", NodeKind::Variable);
}

// ── Global + local in same file: only global survives ───────────────────────

#[test]
fn global_and_local_only_global_emitted() {
    let src = "\
int g_total = 0;\n\
void compute(int param) {\n\
    int local = 42;\n\
    for (int i = 0; i < 10; ++i) {}\n\
}\n";
    let nodes = parse(src);
    find(&nodes, "g_total", NodeKind::Variable);
    absent(&nodes, "param", NodeKind::Variable);
    absent(&nodes, "local", NodeKind::Variable);
    absent(&nodes, "i", NodeKind::Variable);
}
