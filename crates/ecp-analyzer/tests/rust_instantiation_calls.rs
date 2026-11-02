//! Rust has no constructors: `A::new()` is an ordinary associated-function
//! call. A struct literal `A { .. }` constructs the struct, so it calls the
//! `Struct` node (FU-2026-10-30-c0bc9a616111): `ecp impact` on the struct
//! lists the literal sites.

mod instantiation_calls_support;
mod name_ref_support;

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use instantiation_calls_support::{
    assert_accesses_type, assert_calls_type, assert_only_call, graph_of,
};
use name_ref_support::{calls, graph};

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

/// The `Accesses` edge comes from the declared return type; the literal
/// adds one `Calls` edge into the struct.
#[test]
fn test_rust_cross_file_struct_literal_calls_struct() {
    let app = (
        "src/app.rs",
        "use crate::widget::Widget;\n\npub fn make_literal() -> Widget {\n    Widget { x: 1 }\n}\n",
    );
    let graph = graph_of(&provider(), &[WIDGET, app]);
    assert_calls_type(
        &graph,
        "make_literal",
        "Widget",
        NodeKind::Struct,
        "src/widget.rs",
    );
    assert_accesses_type(&graph, "make_literal", "Widget");
}

#[test]
fn test_rust_same_file_struct_literal_calls_struct() {
    let local = (
        "src/local.rs",
        "pub struct Local {\n    pub ready: bool,\n}\n\npub fn make_local() -> Local {\n    Local { ready: true }\n}\n",
    );
    let graph = graph_of(&provider(), &[local]);
    assert_calls_type(
        &graph,
        "make_local",
        "Local",
        NodeKind::Struct,
        "src/local.rs",
    );
}

// FU-2026-10-30-c0bc9a616111 part B: a Rust struct literal constructs its
// struct, so `ecp impact` on the struct lists the functions that build it.
// An enum variant literal (`Shape::Circle { r }`) constructs no struct: it
// never falls back to a project struct named like its last segment.

const VIEW: &str = "pub struct View { pub n: u32 }\n\
                    pub struct Circle { pub r: u32 }\n\
                    pub enum Shape { Circle { r: u32 } }\n\
                    pub struct Wrapper<T> { pub v: T }\n\
                    impl View {\n    pub fn zero() -> Self { Self { n: 0 } }\n}\n";

fn build(user: &str) -> ZeroCopyGraph {
    graph(
        &RustProvider::new().unwrap(),
        &[("src/view.rs", VIEW), ("src/user.rs", user)],
    )
}

fn called(g: &ZeroCopyGraph, caller: &str, name: &str) -> bool {
    calls(g, caller)
        .iter()
        .any(|(file, callee)| file == "src/view.rs" && callee == name)
}

/// Contract: `View { n: 2 }` is a `Calls` edge from the builder to `View`.
#[test]
fn test_struct_literal_binds_struct() {
    let g = build("use crate::view::View;\nfn lit() -> u32 { View { n: 2 }.n }\n");
    assert!(called(&g, "lit", "View"), "{:?}", calls(&g, "lit"));
}

/// Contract: a path as written (`crate::view::View { .. }`) binds the struct.
#[test]
fn test_struct_literal_through_path_binds_struct() {
    let g = build("fn lit() -> u32 { crate::view::View { n: 2 }.n }\n");
    assert!(called(&g, "lit", "View"), "{:?}", calls(&g, "lit"));
}

/// Contract: `Self { .. }` constructs the enclosing impl's type.
#[test]
fn test_self_struct_literal_binds_impl_type() {
    let g = build("");
    assert!(called(&g, "zero", "View"), "{:?}", calls(&g, "zero"));
}

/// Contract: `Wrapper::<u8> { .. }` constructs `Wrapper`; the turbofish is
/// no part of the type.
#[test]
fn test_turbofish_struct_literal_binds_struct() {
    let g = build("use crate::view::Wrapper;\nfn lit() -> u8 { Wrapper::<u8> { v: 1 }.v }\n");
    assert!(called(&g, "lit", "Wrapper"), "{:?}", calls(&g, "lit"));
}

/// Contract (rev #15): an enum variant literal binds no struct, though a
/// project struct `Circle` shares the variant's last segment.
#[test]
fn test_enum_variant_literal_binds_no_same_named_struct() {
    let g = build("use crate::view::Shape;\nfn variant() -> Shape { Shape::Circle { r: 1 } }\n");
    assert!(
        !called(&g, "variant", "Circle"),
        "{:?}",
        calls(&g, "variant")
    );
}

/// Contract: `Self::Variant { .. }` constructs no struct either.
#[test]
fn test_self_variant_literal_binds_no_struct() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/shape.rs",
            "pub struct Circle { pub r: u32 }\n\
             pub enum Shape { Circle { r: u32 } }\n\
             impl Shape {\n    pub fn unit() -> Self { Self::Circle { r: 1 } }\n}\n",
        )],
    );
    assert_eq!(calls(&g, "unit"), Vec::<(String, String)>::new());
}

/// Contract: `Self { .. }` outside an impl (a trait default fn) has no type
/// to bind.
#[test]
fn test_self_literal_outside_impl_binds_nothing() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/t.rs",
            "pub struct Self_;\npub trait T {\n    fn make() -> Self where Self: Sized { Self { } }\n}\n",
        )],
    );
    assert_eq!(calls(&g, "make"), Vec::<(String, String)>::new());
}

/// Contract: a literal of a type the project does not declare binds nothing.
#[test]
fn test_literal_of_undeclared_type_binds_nothing() {
    let g = build("fn lit() { let _ = Missing { n: 1 }; }\n");
    assert_eq!(calls(&g, "lit"), Vec::<(String, String)>::new());
}

/// Guard: a tuple variant call `Shape::Square(2)` binds no
/// struct named `Square` in the enum's file.
#[test]
fn test_tuple_variant_call_binds_no_same_named_free_item() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/view.rs",
                "pub struct Square(pub u32);\npub enum Shape { Square(u32) }\n",
            ),
            (
                "src/user.rs",
                "use crate::view::Shape;\nfn tuple() -> Shape { Shape::Square(2) }\n",
            ),
        ],
    );
    assert_eq!(calls(&g, "tuple"), Vec::<(String, String)>::new());
}
