//! JavaScript instantiation calls: `new A()` must give `ecp impact` a caller
//! for `A`. The call queries do not capture `new_expression`, so today every
//! `new` site is invisible to the graph.

mod instantiation_calls_support;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};

const WIDGET: (&str, &str) = (
    "src/widget.js",
    "export class Widget {\n  constructor(x) {\n    this.x = x;\n  }\n}\n",
);
const GADGET: (&str, &str) = ("src/gadget.js", "export class Gadget {}\n");
const MAKE_WIDGET: (&str, &str) = (
    "src/app.js",
    "import { Widget } from './widget';\n\nexport function makeWidget() {\n  return new Widget(1);\n}\n",
);

fn provider() -> JavaScriptProvider {
    JavaScriptProvider::new().expect("JavaScriptProvider::new")
}

#[test]
fn test_javascript_cross_file_new_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "src/widget.js");
}

#[test]
fn test_javascript_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/app.js",
        "import { Gadget } from './gadget';\n\nexport function makeGadget() {\n  return new Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "src/gadget.js",
    );
}

#[test]
fn test_javascript_same_file_new_calls_constructor() {
    let local = (
        "src/local.js",
        "class Local {\n  constructor() {}\n}\n\nexport function makeLocal() {\n  return new Local();\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "src/local.js");
}

#[test]
fn test_javascript_namespace_import_new_calls_constructor() {
    let app = (
        "src/app.js",
        "import * as shop from './widget';\n\nexport function makeQualified() {\n  return new shop.Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "makeQualified", "Widget", "src/widget.js");
}

/// A function named like an unrelated class keeps its call edge.
#[test]
fn test_javascript_same_named_function_call_targets_function() {
    let util = ("src/util.js", "export function Thing() {\n  return 1;\n}\n");
    let model = ("src/model.js", "export class Thing {}\n");
    let app = (
        "src/app.js",
        "import { Thing } from './util';\n\nexport function callThing() {\n  return Thing();\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "callThing", "Thing", "src/util.js");
}

/// `obj.Widget()` is a method call on an untyped receiver, never a
/// construction of class `Widget`.
#[test]
fn test_javascript_member_call_named_like_class_no_constructor_call() {
    let app = (
        "src/app.js",
        "export function useFactory(obj) {\n  return obj.Widget();\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "useFactory", "Widget");
}
