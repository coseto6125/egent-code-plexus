//! File-scope const/constexpr globals → NodeKind::Const, mutable globals stay
//! NodeKind::Variable.
//!
//! tree-sitter-cpp represents `const int MAX = 100;` as a `declaration` whose
//! first named child is a `type_qualifier` node with text `"const"`.
//! `constexpr double PI = 3.14;` follows the same shape with text
//! `"constexpr"`. Mutable globals (`int counter = 0;`) have no `type_qualifier`
//! child and remain Variable. The `@var` query anchors to
//! `(translation_unit (declaration ...))`, so class fields (`field_declaration`)
//! and function parameters (`parameter_declaration`) are handled by separate
//! capture branches and are unaffected.

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

const SRC: &str = "
const int MAX = 100;
constexpr double PI = 3.14;
int counter = 0;
";

#[test]
fn const_int_global_is_const_kind() {
    let nodes = parse(SRC);
    find(&nodes, "MAX", NodeKind::Const);
}

#[test]
fn constexpr_double_global_is_const_kind() {
    let nodes = parse(SRC);
    find(&nodes, "PI", NodeKind::Const);
}

#[test]
fn mutable_global_stays_variable() {
    let nodes = parse(SRC);
    find(&nodes, "counter", NodeKind::Variable);
}

#[test]
fn const_not_emitted_as_variable() {
    // Guard against over-emission: MAX and PI must not appear as Variable.
    let nodes = parse(SRC);
    absent(&nodes, "MAX", NodeKind::Variable);
    absent(&nodes, "PI", NodeKind::Variable);
}

#[test]
fn mutable_global_not_emitted_as_const() {
    // Guard against over-reclassification: counter must not appear as Const.
    let nodes = parse(SRC);
    absent(&nodes, "counter", NodeKind::Const);
}

#[test]
fn static_const_global_is_const_kind() {
    // `static const` still compile-time constant → Const.
    let nodes = parse("static const int BUF_SIZE = 4096;\n");
    find(&nodes, "BUF_SIZE", NodeKind::Const);
}

#[test]
fn plain_int_global_stays_variable() {
    // Regression: plain mutable global must not be reclassified.
    let nodes = parse("int g_count = 0;\n");
    find(&nodes, "g_count", NodeKind::Variable);
}
