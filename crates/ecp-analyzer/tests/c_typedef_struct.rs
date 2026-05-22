use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let provider = CProvider::new().expect("provider");
    provider
        .parse_file(Path::new("test.c"), src.as_bytes())
        .expect("parse")
}

#[test]
fn test_c_typedef_primitive_emits_typedef_node() {
    let graph = parse("typedef int Foo;\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Foo")
        .unwrap_or_else(|| panic!("expected Typedef node `Foo`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Typedef);
}

#[test]
fn test_c_typedef_struct_emits_struct_and_typedef() {
    let graph = parse("typedef struct Bar { int x; } Bar;\n");
    let struct_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Bar" && n.kind == NodeKind::Struct);
    let typedef_node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Bar" && n.kind == NodeKind::Typedef);
    assert!(
        struct_node.is_some(),
        "expected Struct node `Bar`, got {:#?}",
        graph.nodes
    );
    assert!(
        typedef_node.is_some(),
        "expected Typedef node `Bar`, got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_plain_struct_emits_struct_node() {
    let graph = parse("struct Baz { int y; };\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "Baz")
        .unwrap_or_else(|| panic!("expected Struct node `Baz`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Struct);
}

#[test]
fn test_c_enum_emits_enum_node() {
    let graph = parse("enum E { A, B };\n");
    let node = graph
        .nodes
        .iter()
        .find(|n| n.name == "E")
        .unwrap_or_else(|| panic!("expected Enum node `E`, got {:#?}", graph.nodes));
    assert_eq!(node.kind, NodeKind::Enum);
}

// Regression: tightened struct/union/enum queries must emit ONLY at
// definition sites (with body). Reference forms — forward decls,
// pointer-to-struct in params, sizeof — must not emit duplicate nodes.
//
// Before tightening, every `struct hdr_histogram *h` parameter and every
// `sizeof(struct X)` site over-matched `struct_specifier`, producing
// duplicate Struct emissions that uid-collided with the real definition
// (1,378 collisions on the .sample_repo corpus before this fix).

#[test]
fn test_c_struct_forward_decl_emits_no_struct() {
    let graph = parse("struct OnlyForward;\n");
    assert!(
        !graph.nodes.iter().any(|n| n.name == "OnlyForward"),
        "forward-decl struct (no body) must not emit a Struct node"
    );
}

#[test]
fn test_c_struct_reference_in_param_emits_no_struct() {
    // `struct hdr_histogram *h` as a parameter type is a reference,
    // not a definition. Only the function should be emitted; the
    // `struct hdr_histogram` reference must not over-match.
    let src = "void use(struct hdr_histogram *h) {}\n";
    let graph = parse(src);
    assert!(
        !graph
            .nodes
            .iter()
            .any(|n| n.name == "hdr_histogram" && n.kind == NodeKind::Struct),
        "param-typed struct reference must not emit a Struct node — got {:#?}",
        graph.nodes
    );
}

#[test]
fn test_c_enum_forward_decl_emits_no_enum() {
    // C++/C2x-style scoped enum forward declaration (`enum X : int;`)
    // and plain forward declarations have no body. The tightened query
    // skips them.
    let graph = parse("enum FwdOnly;\n");
    assert!(
        !graph.nodes.iter().any(|n| n.name == "FwdOnly"),
        "forward-decl enum (no body) must not emit an Enum node"
    );
}
