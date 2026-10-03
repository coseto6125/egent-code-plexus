//! C++ instantiation calls: `A(..)` and `new A(..)` must give `ecp impact` a
//! caller for `A`. A declared constructor carries the class name, so the
//! temporary form `A(..)` already resolves; `new A(..)` is not a captured call
//! site, and a class with no constructor, or with overloads, has no single
//! callable named `A`.

mod instantiation_calls_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_calls_constructor, assert_calls_type, assert_ctor_flagged, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "src/widget.hpp",
    "class Widget {\npublic:\n    Widget(int x) : x_(x) {}\n\nprivate:\n    int x_;\n};\n",
);
const GADGET: (&str, &str) = ("src/gadget.hpp", "class Gadget {\n};\n");

fn provider() -> CppProvider {
    CppProvider::new().expect("CppProvider::new")
}

#[test]
fn test_cpp_cross_file_temporary_with_constructor_calls_constructor() {
    let app = (
        "src/app.cpp",
        "#include \"widget.hpp\"\n\nWidget make_temp() {\n    return Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "make_temp", "Widget", "src/widget.hpp");
}

#[test]
fn test_cpp_cross_file_new_with_constructor_calls_constructor() {
    let app = (
        "src/app.cpp",
        "#include \"widget.hpp\"\n\nWidget* make_new() {\n    return new Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_constructor(&graph, "make_new", "Widget", "src/widget.hpp");
}

#[test]
fn test_cpp_cross_file_temporary_without_constructor_calls_class() {
    let app = (
        "src/app.cpp",
        "#include \"gadget.hpp\"\n\nGadget make_gadget() {\n    return Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "make_gadget",
        "Gadget",
        NodeKind::Class,
        "src/gadget.hpp",
    );
}

#[test]
fn test_cpp_cross_file_new_without_constructor_calls_class() {
    let app = (
        "src/app.cpp",
        "#include \"gadget.hpp\"\n\nGadget* make_new_gadget() {\n    return new Gadget();\n}\n",
    );
    let graph = graph_of(&provider(), &[GADGET, app]);
    assert_calls_type(
        &graph,
        "make_new_gadget",
        "Gadget",
        NodeKind::Class,
        "src/gadget.hpp",
    );
}

#[test]
fn test_cpp_same_file_new_calls_constructor() {
    let local = (
        "src/local.cpp",
        "class Local {\npublic:\n    Local() {}\n};\n\nLocal* make_local() {\n    return new Local();\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_constructor(&graph, "make_local", "Local", "src/local.cpp");
}

/// Two overloads: the resolver cannot pick one without argument types, so
/// the edge goes to the class rather than to a guess.
#[test]
fn test_cpp_overloaded_constructors_new_calls_class() {
    let multi = (
        "src/multi.hpp",
        "class Multi {\npublic:\n    Multi(int x) {}\n    Multi(const char* s) {}\n};\n",
    );
    let app = (
        "src/app.cpp",
        "#include \"multi.hpp\"\n\nMulti* make_multi() {\n    return new Multi(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[multi, app]);
    assert_calls_type(
        &graph,
        "make_multi",
        "Multi",
        NodeKind::Class,
        "src/multi.hpp",
    );
}

#[test]
fn test_cpp_namespaced_new_calls_constructor() {
    let item = (
        "src/shop.hpp",
        "namespace shop {\nclass Item {\npublic:\n    Item() {}\n};\n}\n",
    );
    let app = (
        "src/app.cpp",
        "#include \"shop.hpp\"\n\nshop::Item* make_item() {\n    return new shop::Item();\n}\n",
    );
    let graph = graph_of(&provider(), &[item, app]);
    assert_calls_constructor(&graph, "make_item", "Item", "src/shop.hpp");
}

#[test]
fn test_cpp_template_new_calls_constructor() {
    let boxed = (
        "src/box.hpp",
        "template <typename T>\nclass Box {\npublic:\n    Box(T v) : v_(v) {}\n\nprivate:\n    T v_;\n};\n",
    );
    let app = (
        "src/app.cpp",
        "#include \"box.hpp\"\n\nBox<int>* make_box() {\n    return new Box<int>(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[boxed, app]);
    assert_calls_constructor(&graph, "make_box", "Box", "src/box.hpp");
}

/// A free function named like an unrelated class keeps its call edge.
#[test]
fn test_cpp_same_named_function_call_targets_function() {
    let util = ("src/util.cpp", "int Thing() {\n    return 1;\n}\n");
    let model = ("src/thing.hpp", "class Thing {\n};\n");
    let app = (
        "src/app.cpp",
        "int call_thing() {\n    return Thing();\n}\n",
    );
    let graph = graph_of(&provider(), &[util, model, app]);
    assert_only_call(&graph, "call_thing", "Thing", "src/util.cpp");
}

#[test]
fn test_cpp_new_with_constructor_flagged_constructor_call() {
    let app = (
        "src/app.cpp",
        "#include \"widget.hpp\"\n\nWidget* make_new() {\n    return new Widget(1);\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_ctor_flagged(&graph, "make_new", "Widget");
}
