//! Kotlin instantiation calls: `A()` must give `ecp impact` a caller for `A`.
//! A primary constructor carries the class name, so the Callable lookup
//! already finds it. A class with no constructor has no callable named `A`,
//! and the call resolves nothing today.

mod instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = ("src/Widget.kt", "class Widget(val x: Int)\n");
const GADGET: (&str, &str) = ("src/Gadget.kt", "class Gadget {\n}\n");
const MAKE_WIDGET: (&str, &str) = (
    "src/App.kt",
    "fun makeWidget(): Widget {\n    return Widget(1)\n}\n",
);

fn provider() -> KotlinProvider {
    KotlinProvider::new().expect("KotlinProvider::new")
}

#[test]
fn test_kotlin_cross_file_call_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "src/Widget.kt");
}

#[test]
fn test_kotlin_cross_file_call_without_constructor_calls_class() {
    let app = (
        "src/App.kt",
        "fun makeGadget(): Gadget {\n    return Gadget()\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "src/Gadget.kt",
    );
}

#[test]
fn test_kotlin_same_file_call_calls_constructor() {
    let local = (
        "src/Local.kt",
        "class Local(val ready: Boolean)\n\nfun makeLocal(): Local {\n    return Local(true)\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "src/Local.kt");
}

/// Overloads share one uid, so Pass 1 collapses them into one Constructor
/// node: an edge to it means "a constructor of `Multi`", not a guess.
#[test]
fn test_kotlin_overloaded_constructors_call_calls_constructor() {
    let multi = (
        "src/Multi.kt",
        "class Multi {\n    constructor(x: Int) {\n    }\n\n    constructor(s: String) {\n    }\n}\n",
    );
    let app = (
        "src/App.kt",
        "fun makeMulti(): Multi {\n    return Multi(1)\n}\n",
    );
    let graph = graph_of(&provider(), &[multi, app]);
    assert_calls_constructor(&graph, "makeMulti", "Multi", "src/Multi.kt");
}

#[test]
fn test_kotlin_generic_call_calls_constructor() {
    let boxed = ("src/Box.kt", "class Box<T>(val v: T)\n");
    let app = (
        "src/App.kt",
        "fun makeBox(): Box<Int> {\n    return Box<Int>(1)\n}\n",
    );
    let graph = graph_of(&provider(), &[boxed, app]);
    assert_calls_constructor(&graph, "makeBox", "Box", "src/Box.kt");
}

/// A function named like an unrelated class in another package keeps its
/// call edge.
#[test]
fn test_kotlin_same_named_function_call_targets_function() {
    let util = (
        "src/util/Things.kt",
        "package util\n\nfun Thing(): Int {\n    return 1\n}\n",
    );
    let model = ("src/model/Thing.kt", "package model\n\nclass Thing {\n}\n");
    let app = (
        "src/App.kt",
        "import util.Thing\n\nfun callThing(): Int {\n    return Thing()\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "callThing", "Thing", "src/util/Things.kt");
}
