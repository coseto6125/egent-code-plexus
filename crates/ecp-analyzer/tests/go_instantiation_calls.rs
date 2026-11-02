//! Go has no constructors: the `NewA()` factory idiom is an ordinary
//! function call. A composite literal `A{}` constructs the struct, so it
//! calls the `Struct` node (FU-2026-10-30-c0bc9a616111): `ecp impact` on the
//! struct lists the literal sites.

mod instantiation_calls_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::graph::NodeKind;
use instantiation_calls_support::{
    assert_accesses_type, assert_calls_type, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "pkg/widget.go",
    "package shop\n\ntype Widget struct {\n\tX int\n}\n\nfunc NewWidget(x int) *Widget {\n\treturn &Widget{X: x}\n}\n",
);

fn provider() -> GoProvider {
    GoProvider::new().expect("GoProvider::new")
}

/// The `Accesses` edge comes from the declared return type; the literal
/// adds one `Calls` edge into the struct.
#[test]
fn test_go_cross_file_composite_literal_calls_struct() {
    let app = (
        "pkg/app.go",
        "package shop\n\nfunc MakeLiteral() Widget {\n\treturn Widget{X: 1}\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_type(
        &graph,
        "MakeLiteral",
        "Widget",
        NodeKind::Struct,
        "pkg/widget.go",
    );
    assert_accesses_type(&graph, "MakeLiteral", "Widget");
}

#[test]
fn test_go_same_file_composite_literal_calls_struct() {
    let local = (
        "pkg/local.go",
        "package shop\n\ntype Local struct {\n\tReady bool\n}\n\nfunc MakeLocal() Local {\n\treturn Local{Ready: true}\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_type(
        &graph,
        "MakeLocal",
        "Local",
        NodeKind::Struct,
        "pkg/local.go",
    );
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
