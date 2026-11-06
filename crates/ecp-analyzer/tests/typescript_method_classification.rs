//! Verifies that TypeScript class methods are emitted as `NodeKind::Method`
//! and top-level functions remain `NodeKind::Function`.

use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
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
    assert_eq!(
        kind_of(&g, "bar"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
}

#[test]
fn class_static_method_is_method() {
    let g = parse("class Foo { static bar() {} }");
    assert_eq!(
        kind_of(&g, "bar"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
}

#[test]
fn class_async_method_is_method() {
    let g = parse("class Foo { async bar() {} }");
    assert_eq!(
        kind_of(&g, "bar"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
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
    assert_eq!(
        kind_of(&g, "bar"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
    assert_eq!(
        kind_of(&g, "baz"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
}

#[test]
fn abstract_method_signature_is_method() {
    let g = parse("abstract class A { abstract foo(): void; }");
    assert_eq!(
        kind_of(&g, "foo"),
        Some(NodeKind::Method),
        "nodes: {:?}",
        g.nodes
    );
}

#[test]
fn constructor_param_property_is_property() {
    let g = parse(
        "class Foo { constructor(private readonly reflector: Reflector, public x: number) {} }",
    );
    assert_eq!(
        kind_of(&g, "reflector"),
        Some(NodeKind::Property),
        "nodes: {:?}",
        g.nodes
    );
    assert_eq!(
        kind_of(&g, "x"),
        Some(NodeKind::Property),
        "nodes: {:?}",
        g.nodes
    );
}

#[test]
fn const_arrow_fn_is_function_not_const() {
    let g = parse("export const isNil = (val: any): val is null | undefined => val === null;");
    assert_eq!(
        kind_of(&g, "isNil"),
        Some(NodeKind::Function),
        "exported const arrow fn should be Function — nodes: {:?}",
        g.nodes
    );
}

/// Stands for: an interface property signature is a `Property` owned by its
/// interface. Contract change: ref-gitnexus omits interface properties; ecp
/// emits them so an LLM can see the contract field. `meow()` stays a `Method`
/// (POSITIVE CONTROL for the unchanged part) and the class `Dog.name` is a
/// separate node, never mixed with `Cat.name`.
#[test]
fn test_parse_file_typescript_interface_property_signature_emits_property() {
    let g = parse(
        "export class Owner {}\n\
         export interface Cat {\n  readonly id: number;\n  name: string;\n  owner: Owner;\n  meow(): void;\n}\n\
         export class Dog {\n  name: string = '';\n}\n",
    );
    let owned_by = |name: &str, owner: &str| {
        g.nodes
            .iter()
            .filter(|n| n.name == name && n.owner_class.as_deref() == Some(owner))
            .map(|n| n.kind)
            .collect::<Vec<_>>()
    };
    for field in ["id", "name", "owner"] {
        assert_eq!(
            owned_by(field, "Cat"),
            vec![NodeKind::Property],
            "interface property `{field}` — nodes: {:?}",
            g.nodes
        );
    }
    assert_eq!(owned_by("name", "Dog"), vec![NodeKind::Property], "control");
    assert_eq!(
        owned_by("meow", "Cat"),
        vec![NodeKind::Method],
        "control: a method signature stays a Method"
    );
}
