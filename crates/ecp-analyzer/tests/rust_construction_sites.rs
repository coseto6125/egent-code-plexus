//! FU-2026-10-30-c0bc9a616111 part B: a Rust struct literal constructs its
//! struct, so `ecp impact` on the struct lists the functions that build it.
//! An enum variant literal (`Shape::Circle { r }`) constructs no struct: it
//! never falls back to a project struct named like its last segment.
mod name_ref_support;

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::graph::ZeroCopyGraph;
use name_ref_support::{calls, graph};

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
