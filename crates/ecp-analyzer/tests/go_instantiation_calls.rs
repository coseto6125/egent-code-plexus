//! Go has no constructors: a composite literal `A{}` is not a call, and the
//! `NewA()` factory idiom is an ordinary function call. These tests pin that
//! behaviour so the constructor-call change leaves Go unchanged.

mod instantiation_calls_support;

use ecp_analyzer::go::parser::GoProvider;
use instantiation_calls_support::{
    assert_accesses_type, assert_no_instantiation_call, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "pkg/widget.go",
    "package shop\n\ntype Widget struct {\n\tX int\n}\n\nfunc NewWidget(x int) *Widget {\n\treturn &Widget{X: x}\n}\n",
);

fn provider() -> GoProvider {
    GoProvider::new().expect("GoProvider::new")
}

/// The `Accesses` edge comes from the declared return type, not from the
/// literal; the contract here is "no Calls edge into the struct".
#[test]
fn test_go_cross_file_composite_literal_no_calls_edge() {
    let app = (
        "pkg/app.go",
        "package shop\n\nfunc MakeLiteral() Widget {\n\treturn Widget{X: 1}\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "MakeLiteral", "Widget");
    assert_accesses_type(&graph, "MakeLiteral", "Widget");
}

#[test]
fn test_go_same_file_composite_literal_no_calls_edge() {
    let local = (
        "pkg/local.go",
        "package shop\n\ntype Local struct {\n\tReady bool\n}\n\nfunc MakeLocal() Local {\n\treturn Local{Ready: true}\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_no_instantiation_call(&graph, "MakeLocal", "Local");
}

#[test]
fn test_go_factory_function_call_targets_function() {
    let app = (
        "pkg/app.go",
        "package shop\n\nfunc MakeViaFactory() *Widget {\n\treturn NewWidget(1)\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_only_call(&graph, "MakeViaFactory", "NewWidget", "pkg/widget.go");
}
