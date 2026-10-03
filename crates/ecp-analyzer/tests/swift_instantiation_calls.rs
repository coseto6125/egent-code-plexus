//! Swift instantiation calls: `A()` must give `ecp impact` a caller for `A`.
//! The initializer is `init`, so the Callable lookup for `A` finds only the
//! type and today resolves nothing.

mod instantiation_calls_support;

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_ctor_flagged, assert_no_instantiation_call,
    assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "Sources/Widget.swift",
    "class Widget {\n    let x: Int\n\n    init(x: Int) {\n        self.x = x\n    }\n}\n",
);
const GADGET: (&str, &str) = ("Sources/Gadget.swift", "class Gadget {\n}\n");
const MAKE_WIDGET: (&str, &str) = (
    "Sources/App.swift",
    "func makeWidget() -> Widget {\n    return Widget(x: 1)\n}\n",
);

fn provider() -> SwiftProvider {
    SwiftProvider::new().expect("SwiftProvider::new")
}

#[test]
fn test_swift_cross_file_call_with_init_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "Sources/Widget.swift");
}

#[test]
fn test_swift_cross_file_call_without_init_calls_class() {
    let app = (
        "Sources/App.swift",
        "func makeGadget() -> Gadget {\n    return Gadget()\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "Sources/Gadget.swift",
    );
}

#[test]
fn test_swift_same_file_call_calls_constructor() {
    let local = (
        "Sources/Local.swift",
        "class Local {\n    init() {\n    }\n}\n\nfunc makeLocal() -> Local {\n    return Local()\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "Sources/Local.swift");
}

/// Two initializers: the resolver cannot pick one without argument labels,
/// so the edge goes to the class rather than to a guess.
#[test]
fn test_swift_overloaded_inits_call_calls_class() {
    let multi = (
        "Sources/Multi.swift",
        "class Multi {\n    init(x: Int) {\n    }\n\n    init(s: String) {\n    }\n}\n",
    );
    let app = (
        "Sources/App.swift",
        "func makeMulti() -> Multi {\n    return Multi(x: 1)\n}\n",
    );
    let graph = graph_of(&provider(), &[multi, app]);
    assert_calls_type(
        &graph,
        "makeMulti",
        "Multi",
        NodeKind::Class,
        "Sources/Multi.swift",
    );
}

/// A struct with no declared `init` gets the memberwise initializer, which
/// has no node: the edge goes to the struct.
#[test]
fn test_swift_struct_memberwise_init_call_calls_struct() {
    let point = ("Sources/Point.swift", "struct Point {\n    let x: Int\n}\n");
    let app = (
        "Sources/App.swift",
        "func makePoint() -> Point {\n    return Point(x: 1)\n}\n",
    );
    let graph = graph_of(&provider(), &[point, app]);
    assert_calls_type(
        &graph,
        "makePoint",
        "Point",
        NodeKind::Struct,
        "Sources/Point.swift",
    );
}

/// `f.Widget()` is a method call, never a construction of class `Widget`.
#[test]
fn test_swift_member_call_named_like_class_targets_method() {
    let factory = (
        "Sources/Factory.swift",
        "class Factory {\n    func Widget() -> Int {\n        return 1\n    }\n}\n",
    );
    let app = (
        "Sources/App.swift",
        "func useFactory(f: Factory) -> Int {\n    return f.Widget()\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, factory, app]);
    assert_no_instantiation_call(&graph, "useFactory", "Widget");
    assert_only_call(&graph, "useFactory", "Widget", "Sources/Factory.swift");
}

#[test]
fn test_swift_call_with_init_flagged_constructor_call() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_ctor_flagged(&graph, "makeWidget", "Widget");
}
