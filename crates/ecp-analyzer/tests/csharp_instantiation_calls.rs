//! C# instantiation calls: `new A()` must give `ecp impact` a caller for `A`.
//! A declared constructor carries the class name, so the Callable lookup
//! already finds it. A class with no constructor has no callable named `A`,
//! and the call resolves nothing today.

mod instantiation_calls_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_no_instantiation_call, assert_only_call,
    graph_of,
};

const WIDGET: (&str, &str) = (
    "src/Widget.cs",
    "public class Widget\n{\n    public Widget(int x)\n    {\n    }\n}\n",
);
const GADGET: (&str, &str) = ("src/Gadget.cs", "public class Gadget\n{\n}\n");
const MAKE_WIDGET: (&str, &str) = (
    "src/App.cs",
    "public class App\n{\n    public Widget MakeWidget()\n    {\n        return new Widget(1);\n    }\n}\n",
);

fn provider() -> CSharpProvider {
    CSharpProvider::new().expect("CSharpProvider::new")
}

#[test]
fn test_csharp_cross_file_new_with_constructor_calls_constructor() {
    let graph = graph_of(&provider(), &[WIDGET, MAKE_WIDGET]);
    assert_calls_constructor(&graph, "MakeWidget", "Widget", "src/Widget.cs");
}

#[test]
fn test_csharp_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/App.cs",
        "public class App\n{\n    public Gadget MakeGadget()\n    {\n        return new Gadget();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "MakeGadget",
        "Gadget",
        NodeKind::Class,
        "src/Gadget.cs",
    );
}

#[test]
fn test_csharp_same_file_new_calls_constructor() {
    let local = (
        "src/Local.cs",
        "class Local\n{\n    public Local()\n    {\n    }\n}\n\nclass LocalApp\n{\n    Local MakeLocal()\n    {\n        return new Local();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "MakeLocal", "Local", "src/Local.cs");
}

/// Overloads share one uid, so Pass 1 collapses them into one Constructor
/// node: an edge to it means "a constructor of `Multi`", not a guess.
#[test]
fn test_csharp_overloaded_constructors_new_calls_constructor() {
    let multi = (
        "src/Multi.cs",
        "public class Multi\n{\n    public Multi(int x)\n    {\n    }\n\n    public Multi(string s)\n    {\n    }\n}\n",
    );
    let app = (
        "src/App.cs",
        "public class App\n{\n    public Multi MakeMulti()\n    {\n        return new Multi(1);\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[multi, app]);
    assert_calls_constructor(&graph, "MakeMulti", "Multi", "src/Multi.cs");
}

#[test]
fn test_csharp_generic_new_calls_constructor() {
    let boxed = (
        "src/Box.cs",
        "public class Box<T>\n{\n    public Box(T v)\n    {\n    }\n}\n",
    );
    let app = (
        "src/App.cs",
        "public class App\n{\n    public Box<int> MakeBox()\n    {\n        return new Box<int>(1);\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[boxed, app]);
    assert_calls_constructor(&graph, "MakeBox", "Box", "src/Box.cs");
}

/// A method named like an unrelated class keeps its call edge.
#[test]
fn test_csharp_same_named_method_call_targets_method() {
    let thing = ("src/Thing.cs", "public class Thing\n{\n}\n");
    let caller = (
        "src/Caller.cs",
        "public class Caller\n{\n    int Thing()\n    {\n        return 1;\n    }\n\n    int CallThing()\n    {\n        return Thing();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[thing, caller]);
    assert_only_call(&graph, "CallThing", "Thing", "src/Caller.cs");
}

/// `f.Widget()` is a method call, never a construction of class `Widget`,
/// although `Widget`'s constructor is a callable of the same name.
#[test]
fn test_csharp_member_call_named_like_class_targets_method() {
    let factory = (
        "src/Factory.cs",
        "public class Factory\n{\n    public int Widget()\n    {\n        return 1;\n    }\n}\n",
    );
    let app = (
        "src/App.cs",
        "public class App\n{\n    public int UseFactory(Factory f)\n    {\n        return f.Widget();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, factory, app]);
    assert_only_call(&graph, "UseFactory", "Widget", "src/Factory.cs");
}

/// `GetFactory().Customer()` calls a method on an untyped receiver; only
/// `new Customer()` constructs in C#.
#[test]
fn test_csharp_untyped_member_call_named_like_class_no_constructor_call() {
    let customer = ("src/Customer.cs", "public class Customer\n{\n}\n");
    let app = (
        "src/App.cs",
        "public class App\n{\n    public void Use()\n    {\n        GetFactory().Customer();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[customer, app]);
    assert_no_instantiation_call(&graph, "Use", "Customer");
}

/// Generic arguments in the middle of the path: `Outer<int>.Inner` names
/// `Inner`, never `Outer`.
#[test]
fn test_csharp_new_nested_type_of_generic_outer_calls_inner() {
    let outer = (
        "src/Outer.cs",
        "public class Outer<T>\n{\n    public class Inner\n    {\n    }\n}\n",
    );
    let app = (
        "src/App.cs",
        "public class App\n{\n    public object MakeInner()\n    {\n        return new Outer<int>.Inner();\n    }\n}\n",
    );
    let graph = graph_of(&provider(), &[outer, app]);
    assert_calls_type(
        &graph,
        "MakeInner",
        "Inner",
        NodeKind::Class,
        "src/Outer.cs",
    );
    assert_no_instantiation_call(&graph, "MakeInner", "Outer");
}
