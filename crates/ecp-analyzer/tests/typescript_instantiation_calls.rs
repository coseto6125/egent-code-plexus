//! TypeScript instantiation calls: `new A()` must give `ecp impact` a caller
//! for `A`. The call queries do not capture `new_expression`, so today every
//! `new` site is invisible to the graph.

mod instantiation_calls_support;

use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};

const WIDGET: (&str, &str) = (
    "src/widget.ts",
    "export class Widget {\n  constructor(public x: number) {}\n}\n",
);
const GADGET: (&str, &str) = ("src/gadget.ts", "export class Gadget {}\n");

fn provider() -> TypeScriptProvider {
    TypeScriptProvider::new().expect("TypeScriptProvider::new")
}

#[test]
fn test_typescript_cross_file_new_with_constructor_calls_constructor() {
    let app = (
        "src/app.ts",
        "import { Widget } from './widget';\n\nexport function makeWidget(): Widget {\n  return new Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "src/widget.ts");
}

#[test]
fn test_typescript_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/app.ts",
        "import { Gadget } from './gadget';\n\nexport function makeGadget(): Gadget {\n  return new Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "src/gadget.ts",
    );
}

#[test]
fn test_typescript_same_file_new_calls_constructor() {
    let local = (
        "src/local.ts",
        "class Local {\n  constructor() {}\n}\n\nexport function makeLocal(): Local {\n  return new Local();\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "src/local.ts");
}

#[test]
fn test_typescript_namespace_import_new_calls_constructor() {
    let app = (
        "src/app.ts",
        "import * as shop from './widget';\n\nexport function makeQualified() {\n  return new shop.Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "makeQualified", "Widget", "src/widget.ts");
}

#[test]
fn test_typescript_generic_new_calls_constructor() {
    let boxed = (
        "src/box.ts",
        "export class Box<T> {\n  constructor(public v: T) {}\n}\n",
    );
    let app = (
        "src/app.ts",
        "import { Box } from './box';\n\nexport function makeBox() {\n  return new Box<number>(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[boxed, app]);
    assert_calls_constructor(&graph, "makeBox", "Box", "src/box.ts");
}

/// A function named like an unrelated class keeps its call edge.
#[test]
fn test_typescript_same_named_function_call_targets_function() {
    let util = (
        "src/util.ts",
        "export function Thing(): number {\n  return 1;\n}\n",
    );
    let model = ("src/model.ts", "export class Thing {}\n");
    let app = (
        "src/app.ts",
        "import { Thing } from './util';\n\nexport function callThing(): number {\n  return Thing();\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "callThing", "Thing", "src/util.ts");
}

/// `f.Widget()` is a method call, never a construction of class `Widget`.
#[test]
fn test_typescript_member_call_named_like_class_targets_method() {
    let factory = (
        "src/factory.ts",
        "export class Factory {\n  Widget(): number {\n    return 1;\n  }\n}\n",
    );
    let app = (
        "src/app.ts",
        "import { Factory } from './factory';\n\nexport function useFactory(f: Factory): number {\n  return f.Widget();\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, factory, app]);
    assert_no_instantiation_call(&graph, "useFactory", "Widget");
    assert_only_call(&graph, "useFactory", "Widget", "src/factory.ts");
}
