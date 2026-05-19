//! TypeScript `abstract class X { }` declarations must emit as Class.
//! tree-sitter-typescript uses a dedicated `abstract_class_declaration`
//! node (not a subclass of `class_declaration`), so the regular class
//! pattern doesn't fire. Previously cgn-rs missed every abstract class in
//! NestJS source (AbstractHttpAdapter, ClientProxy, Server, ContextCreator,
//! ModuleRef, etc.) — 17 unpaired ref_over entries on the parity dump.

use cgn_analyzer::typescript::parser::TypeScriptProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::{LocalGraph, RawNode};
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = TypeScriptProvider::new().expect("TypeScriptProvider init");
    p.parse_file(Path::new("t.ts"), src.as_bytes()).expect("parse_file")
}

fn classes(g: &LocalGraph) -> Vec<&RawNode> {
    g.nodes.iter().filter(|n| n.kind == NodeKind::Class).collect()
}

#[test]
fn abstract_class_emits_class() {
    let g = parse("abstract class AbstractAdapter {}\n");
    let cs = classes(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "AbstractAdapter");
}

#[test]
fn exported_abstract_class_emits_class() {
    let g = parse("export abstract class ClientProxy {}\n");
    let cs = classes(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "ClientProxy");
    assert!(cs[0].is_exported, "export abstract class must be exported");
}

#[test]
fn abstract_class_with_extends_emits_class_and_heritage() {
    let g = parse(
        "export abstract class ModuleRef extends AbstractInstanceResolver {}\n",
    );
    let cs = classes(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "ModuleRef");
    assert!(
        cs[0].heritage.iter().any(|h| h == "AbstractInstanceResolver"),
        "expected heritage AbstractInstanceResolver, got {:?}",
        cs[0].heritage,
    );
}

#[test]
fn abstract_class_with_generics_emits_class() {
    let g = parse("export abstract class AbstractHttpAdapter<TInstance, TRequest, TResponse> {}\n");
    let cs = classes(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "AbstractHttpAdapter");
}

#[test]
fn concrete_class_still_emits_class_after_abstract_added() {
    // Regression: adding abstract_class_declaration capture must not break
    // the existing class_declaration path.
    let g = parse("export class Foo { bar() {} }\n");
    let cs = classes(&g);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].name, "Foo");
}
