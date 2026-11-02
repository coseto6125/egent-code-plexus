//! FU-2026-10-30-c0bc9a616111 part A: a declaring-type keyword annotation
//! (`-> Self`) stands for the type that owns the member, so `ecp impact` on
//! the struct lists the functions that return it. The keyword is no type
//! name: it binds through the owner, never through a project type spelled
//! `Self`.
mod name_ref_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::graph::RelType;
use name_ref_support::{graph, refs};

const TYPE_ANNOTATION: &str = "type_annotation";

/// `(target file, target name)` of every `type_annotation` edge out of `source`.
fn annotated(g: &ecp_core::graph::ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, TYPE_ANNOTATION)
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: `fn new() -> Self` reaches its impl type, like `-> Resolver`.
#[test]
fn test_self_return_annotation_binds_impl_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/table.rs",
            "pub struct Resolver { pub n: u32 }\n\
             impl Resolver {\n    pub fn new() -> Self { Resolver { n: 0 } }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "new"), [pair("src/table.rs", "Resolver")]);
}

/// Contract: a `&mut Self` return keeps the type's identity.
#[test]
fn test_self_return_annotation_through_reference_binds_impl_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/table.rs",
            "pub struct Resolver { pub n: u32 }\n\
             impl Resolver {\n    pub fn bump(&mut self) -> &mut Self { self }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "bump"), [pair("src/table.rs", "Resolver")]);
}

/// Contract: a generic impl's `Self` is the struct, not `Wrapper<T>`.
#[test]
fn test_self_return_annotation_in_generic_impl_binds_struct() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/wrap.rs",
            "pub struct Wrapper<T> { pub v: T }\n\
             impl<T> Wrapper<T> {\n    pub fn make(v: T) -> Self { Wrapper { v } }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "make"), [pair("src/wrap.rs", "Wrapper")]);
}

/// Contract (stated no-edge): `Option<Self>` / `Result<Self, E>` need their
/// own unwrapping rules; a guess would bind the wrong member.
#[test]
fn test_self_inside_generic_return_binds_no_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/table.rs",
            "pub struct Resolver;\n\
             impl Resolver {\n\
             \x20   pub fn maybe() -> Option<Self> { None }\n\
             \x20   pub fn fallible() -> Result<Self, ()> { Err(()) }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "maybe"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "fallible"), Vec::<(String, String)>::new());
}

/// Contract: two types in two files each bind their own `Self`.
#[test]
fn test_self_return_annotation_same_name_in_two_types_binds_each_owner() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[
            (
                "src/a.rs",
                "pub struct Alpha;\nimpl Alpha {\n    pub fn new() -> Self { Alpha }\n}\n",
            ),
            (
                "src/b.rs",
                "pub struct Beta;\nimpl Beta {\n    pub fn new() -> Self { Beta }\n}\n",
            ),
        ],
    );
    assert_eq!(
        annotated(&g, "new"),
        [pair("src/a.rs", "Alpha"), pair("src/b.rs", "Beta")]
    );
}

/// Contract: a function with no return type has no annotation edge.
#[test]
fn test_function_without_return_type_has_no_annotation_edge() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/table.rs",
            "pub struct Resolver;\nimpl Resolver {\n    pub fn reset(&mut self) {}\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "reset"), Vec::<(String, String)>::new());
}

/// Contract (rev #14): the keyword is gated by language. A C++ `Self` is an
/// ordinary type, so the field binds the struct named `Self`, not `Box`.
#[test]
fn test_cpp_field_typed_with_a_struct_named_self_binds_that_struct() {
    let g = graph(
        &CppProvider::new().unwrap(),
        &[(
            "src/box.cpp",
            "struct Self {};\nstruct Box { Self value; };\n",
        )],
    );
    assert_eq!(annotated(&g, "value"), [pair("src/box.cpp", "Self")]);
}
