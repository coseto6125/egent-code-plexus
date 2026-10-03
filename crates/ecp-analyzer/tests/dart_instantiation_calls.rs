//! Dart instantiation calls: `A()` must give `ecp impact` a caller for `A`.
//! A declared constructor carries the class name, so the Callable lookup
//! already finds it. A class with no constructor has no callable named `A`,
//! and the call resolves nothing today.

mod instantiation_calls_support;

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_ctor_flagged, assert_no_instantiation_call,
    assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "lib/widget.dart",
    "class Widget {\n  final int x;\n\n  Widget(this.x);\n}\n",
);
const GADGET: (&str, &str) = ("lib/gadget.dart", "class Gadget {}\n");
const MAKE_WIDGET: (&str, &str) = (
    "lib/app.dart",
    "import 'widget.dart';\n\nWidget makeWidget() {\n  return Widget(1);\n}\n",
);

fn provider() -> DartProvider {
    DartProvider::new().expect("DartProvider::new")
}

#[test]
fn test_dart_cross_file_call_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "lib/widget.dart");
}

#[test]
fn test_dart_cross_file_call_without_constructor_calls_class() {
    let app = (
        "lib/app.dart",
        "import 'gadget.dart';\n\nGadget makeGadget() {\n  return Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "lib/gadget.dart",
    );
}

#[test]
fn test_dart_same_file_call_calls_constructor() {
    let local = (
        "lib/local.dart",
        "class Local {\n  Local();\n}\n\nLocal makeLocal() {\n  return Local();\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "lib/local.dart");
}

/// A function named like an unrelated class in another library keeps its
/// call edge.
#[test]
fn test_dart_same_named_function_call_targets_function() {
    let util = ("lib/util.dart", "int Thing() {\n  return 1;\n}\n");
    let model = ("lib/thing.dart", "class Thing {}\n");
    let app = (
        "lib/app.dart",
        "import 'util.dart';\n\nint callThing() {\n  return Thing();\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "callThing", "Thing", "lib/util.dart");
}

/// `f.Widget()` is a method call, never a construction of class `Widget`,
/// although `Widget`'s constructor is a callable of the same name.
#[test]
fn test_dart_member_call_named_like_class_targets_method() {
    let factory = (
        "lib/factory.dart",
        "class Factory {\n  int Widget() {\n    return 1;\n  }\n}\n",
    );
    let app = (
        "lib/app.dart",
        "import 'factory.dart';\n\nint useFactory(Factory f) {\n  return f.Widget();\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, factory, app]);
    assert_no_instantiation_call(&graph, "useFactory", "Widget");
    assert_only_call(&graph, "useFactory", "Widget", "lib/factory.dart");
}

#[test]
fn test_dart_call_with_constructor_flagged_constructor_call() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_ctor_flagged(&graph, "makeWidget", "Widget");
}
