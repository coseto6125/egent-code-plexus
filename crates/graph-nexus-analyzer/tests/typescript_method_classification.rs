//! Verifies that TypeScript class methods are emitted as `NodeKind::Method`
//! and top-level functions remain `NodeKind::Function`.

use graph_nexus_analyzer::typescript::TypeScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.ts"), src.as_bytes())
        .expect("parse")
}

fn kind_of(g: &LocalGraph, name: &str) -> Option<NodeKind> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn class_instance_method_is_method() {
    let g = parse("class Foo { bar() {} }");
    assert_eq!(kind_of(&g, "bar"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
}

#[test]
fn class_static_method_is_method() {
    let g = parse("class Foo { static bar() {} }");
    assert_eq!(kind_of(&g, "bar"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
}

#[test]
fn class_async_method_is_method() {
    let g = parse("class Foo { async bar() {} }");
    assert_eq!(kind_of(&g, "bar"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
}

#[test]
fn top_level_function_is_function() {
    let g = parse("function topLevel() {}");
    assert_eq!(
        kind_of(&g, "topLevel"),
        Some(NodeKind::Function),
        "nodes: {:?}",
        g.nodes
    );
}

/// Arrow-function class fields (`bar = () => {}`) are stored as
/// `public_field_definition` in the tree-sitter AST, not `method_definition`.
/// The parser emits them as `NodeKind::Property` (via the `@property` capture).
/// This is intentional: they are class properties whose value happens to be a
/// function, not syntactic methods.  Arrow fields don't participate in
/// prototype chains and aren't overridable — treating them as Property is the
/// correct signal for graph consumers.
#[test]
fn class_arrow_field_is_property() {
    let g = parse("class Foo { bar = () => {} }");
    assert_eq!(
        kind_of(&g, "bar"),
        Some(NodeKind::Property),
        "arrow fields are Property, not Method — nodes: {:?}",
        g.nodes
    );
}

#[test]
fn interface_method_signature_is_method() {
    let g = parse("export interface Foo { bar(): void; baz?(x: number): string; }");
    assert_eq!(kind_of(&g, "bar"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
    assert_eq!(kind_of(&g, "baz"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
}

#[test]
fn abstract_method_signature_is_method() {
    let g = parse("abstract class A { abstract foo(): void; }");
    assert_eq!(kind_of(&g, "foo"), Some(NodeKind::Method), "nodes: {:?}", g.nodes);
}

#[test]
fn constructor_param_property_is_property() {
    let g = parse(
        "class Foo { constructor(private readonly reflector: Reflector, public x: number) {} }",
    );
    assert_eq!(kind_of(&g, "reflector"), Some(NodeKind::Property), "nodes: {:?}", g.nodes);
    assert_eq!(kind_of(&g, "x"), Some(NodeKind::Property), "nodes: {:?}", g.nodes);
}

#[test]
fn const_arrow_fn_is_function_not_const() {
    let g = parse(
        "export const isNil = (val: any): val is null | undefined => val === null;",
    );
    assert_eq!(
        kind_of(&g, "isNil"),
        Some(NodeKind::Function),
        "exported const arrow fn should be Function — nodes: {:?}",
        g.nodes
    );
}

#[test]
fn interface_property_signature_not_emitted() {
    // Interface property signatures should NOT be emitted as Property —
    // ref-gitnexus omits them; only public_field_definition and constructor
    // parameter properties are emitted.
    let g = parse("export interface Cat { readonly id: number; name: string; }");
    assert_eq!(
        kind_of(&g, "id"),
        None,
        "interface property_signature must not be emitted — nodes: {:?}",
        g.nodes
    );
}
