//! Rust has no constructors: `A::new()` is an ordinary associated-function
//! call, and a struct literal `A { .. }` is not a call. These tests pin that
//! behaviour so the constructor-call change leaves Rust unchanged.

mod instantiation_calls_support;

use ecp_analyzer::rust::parser::RustProvider;
use instantiation_calls_support::{
    assert_accesses_type, assert_no_instantiation_call, assert_only_call, graph_of,
};

const WIDGET: (&str, &str) = (
    "src/widget.rs",
    "pub struct Widget {\n    pub x: i32,\n}\n\nimpl Widget {\n    pub fn new(x: i32) -> Self {\n        Widget { x }\n    }\n}\n",
);

fn provider() -> RustProvider {
    RustProvider::new().expect("RustProvider::new")
}

#[test]
fn test_rust_associated_new_call_targets_new() {
    let app = (
        "src/app.rs",
        "use crate::widget::Widget;\n\npub fn make_via_new() -> Widget {\n    Widget::new(1)\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_only_call(&graph, "make_via_new", "new", "src/widget.rs");
}

/// The `Accesses` edge comes from the declared return type, not from the
/// literal; the contract here is "no Calls edge into the struct".
#[test]
fn test_rust_cross_file_struct_literal_no_calls_edge() {
    let app = (
        "src/app.rs",
        "use crate::widget::Widget;\n\npub fn make_literal() -> Widget {\n    Widget { x: 1 }\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_no_instantiation_call(&graph, "make_literal", "Widget");
    assert_accesses_type(&graph, "make_literal", "Widget");
}

#[test]
fn test_rust_same_file_struct_literal_no_calls_edge() {
    let local = (
        "src/local.rs",
        "pub struct Local {\n    pub ready: bool,\n}\n\npub fn make_local() -> Local {\n    Local { ready: true }\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_no_instantiation_call(&graph, "make_local", "Local");
}
