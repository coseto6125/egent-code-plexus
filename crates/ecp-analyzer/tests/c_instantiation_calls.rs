//! C has no constructors: a struct initializer `struct A a = { .. }` is not a
//! call, and a `make_a()` factory is an ordinary function call. These tests
//! pin that behaviour so the constructor-call change leaves C unchanged.

mod instantiation_calls_support;

use ecp_analyzer::c::parser::CProvider;
use instantiation_calls_support::{
    assert_no_instantiation_call, assert_only_call, calls_from, graph_of,
};

const WIDGET: (&str, &str) = (
    "src/widget.c",
    "struct Widget {\n    int x;\n};\n\nstruct Widget make_widget(int x) {\n    struct Widget w = { x };\n    return w;\n}\n",
);

fn provider() -> CProvider {
    CProvider::new().expect("CProvider::new")
}

#[test]
fn test_c_cross_file_struct_initializer_no_calls_edge() {
    let app = (
        "src/app.c",
        "int use_literal(void) {\n    struct Widget w = { 1 };\n    return w.x;\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "use_literal", "Widget");
    let calls = calls_from(&graph, "use_literal");
    assert!(calls.is_empty(), "use_literal makes no call; got {calls:?}");
}

#[test]
fn test_c_same_file_struct_initializer_no_calls_edge() {
    let local = (
        "src/local.c",
        "struct Local {\n    int ready;\n};\n\nint make_local(void) {\n    struct Local l = { 1 };\n    return l.ready;\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_no_instantiation_call(&graph, "make_local", "Local");
}

#[test]
fn test_c_factory_function_call_targets_function() {
    let app = (
        "src/app.c",
        "int use_factory(void) {\n    struct Widget w = make_widget(1);\n    return w.x;\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_only_call(&graph, "use_factory", "make_widget", "src/widget.c");
}
