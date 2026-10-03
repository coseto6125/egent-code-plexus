//! Java instantiation calls: `new A()` must give `ecp impact` a caller for
//! `A`. A declared constructor carries the class name, so the Callable lookup
//! already finds it. A class with no constructor, or with overloads, has no
//! single callable named `A`, and the call resolves nothing today.

mod instantiation_calls_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_ctor_flagged, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "src/Widget.java",
    "public class Widget {\n    private final int x;\n\n    public Widget(int x) {\n        this.x = x;\n    }\n}\n",
);
const GADGET: (&str, &str) = ("src/Gadget.java", "public class Gadget {\n}\n");
const MAKE_WIDGET: (&str, &str) = (
    "src/App.java",
    "public class App {\n    public Widget makeWidget() {\n        return new Widget(1);\n    }\n}\n",
);

fn provider() -> JavaProvider {
    JavaProvider::new().expect("JavaProvider::new")
}

#[test]
fn test_java_cross_file_new_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "makeWidget", "Widget", "src/Widget.java");
}

#[test]
fn test_java_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/App.java",
        "public class App {\n    public Gadget makeGadget() {\n        return new Gadget();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "makeGadget",
        "Gadget",
        NodeKind::Class,
        "src/Gadget.java",
    );
}

#[test]
fn test_java_same_file_new_calls_constructor() {
    let local = (
        "src/Local.java",
        "class Local {\n    Local() {\n    }\n}\n\nclass LocalApp {\n    Local makeLocal() {\n        return new Local();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "makeLocal", "Local", "src/Local.java");
}

/// Two overloads: the resolver cannot pick one without argument types, so
/// the edge goes to the class rather than to a guess.
#[test]
fn test_java_overloaded_constructors_new_calls_class() {
    let multi = (
        "src/Multi.java",
        "public class Multi {\n    public Multi(int x) {\n    }\n\n    public Multi(String s) {\n    }\n}\n",
    );
    let app = (
        "src/App.java",
        "public class App {\n    public Multi makeMulti() {\n        return new Multi(1);\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[multi, app]);
    assert_calls_type(
        &graph,
        "makeMulti",
        "Multi",
        NodeKind::Class,
        "src/Multi.java",
    );
}

#[test]
fn test_java_generic_new_calls_constructor() {
    let boxed = (
        "src/Box.java",
        "public class Box<T> {\n    private final T v;\n\n    public Box(T v) {\n        this.v = v;\n    }\n}\n",
    );
    let app = (
        "src/App.java",
        "public class App {\n    public Box<String> makeBox() {\n        return new Box<String>(\"x\");\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[boxed, app]);
    assert_calls_constructor(&graph, "makeBox", "Box", "src/Box.java");
}

/// A method named like an unrelated class keeps its call edge.
#[test]
fn test_java_same_named_method_call_targets_method() {
    let thing = ("src/Thing.java", "public class Thing {\n}\n");
    let caller = (
        "src/Caller.java",
        "public class Caller {\n    int Thing() {\n        return 1;\n    }\n\n    int callThing() {\n        return Thing();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[thing, caller]);
    assert_only_call(&graph, "callThing", "Thing", "src/Caller.java");
}

/// `f.Widget()` is a method call, never a construction of class `Widget`,
/// although `Widget`'s constructor is a callable of the same name.
#[test]
fn test_java_member_call_named_like_class_targets_method() {
    let factory = (
        "src/Factory.java",
        "public class Factory {\n    public int Widget() {\n        return 1;\n    }\n}\n",
    );
    let app = (
        "src/App.java",
        "public class App {\n    public int useFactory(Factory f) {\n        return f.Widget();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, factory, app]);
    assert_only_call(&graph, "useFactory", "Widget", "src/Factory.java");
}

#[test]
fn test_java_new_with_constructor_flagged_constructor_call() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_ctor_flagged(&graph, "makeWidget", "Widget");
}
