//! FU-2026-10-30-c0bc9a616111 part A: a declaring-type keyword annotation
//! (`-> Self`) stands for the type that owns the member, so `ecp impact` on
//! the struct lists the functions that return it. The keyword is no type
//! name: it binds through the owner, never through a project type spelled
//! `Self`.
use crate::name_ref_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::rust::parser::RustProvider;
use name_ref_support::{annotated, graph};

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

/// Contract: a `&Tok` / `&mut Tok` / `&'a Tok` return binds the project
/// struct `Tok`. The `return_type` capture keeps the reference text. Before the
/// change the raw text `&Tok` names no type: no edge. `Vec<Tok>` binds `Tok`
/// as its element type. With no project `Tok` nothing binds.
#[test]
fn test_reference_return_annotation_binds_project_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/app.rs",
            "pub struct Tok;\n\
             pub fn peek(t: &Tok) -> &Tok { t }\n\
             pub fn peek_mut(t: &mut Tok) -> &mut Tok { t }\n\
             pub fn lifetime<'a>(t: &'a Tok) -> &'a Tok { t }\n\
             pub fn all() -> Vec<Tok> { Vec::new() }\n",
        )],
    );
    for name in ["peek", "peek_mut", "lifetime"] {
        assert_eq!(annotated(&g, name), [pair("src/app.rs", "Tok")], "{name}");
    }
    assert_eq!(annotated(&g, "all"), [pair("src/app.rs", "Tok")]);

    let g = graph(
        &RustProvider::new().unwrap(),
        &[("src/app.rs", "pub fn peek(t: &Tok) -> &Tok { t }\n")],
    );
    assert_eq!(annotated(&g, "peek"), Vec::<(String, String)>::new());
}

/// Contract: a struct field typed with a project struct reaches that struct,
/// like a parameter does, so `ecp impact` on `St` lists the field. A field
/// typed with a builtin or the struct's own type parameter binds nothing.
#[test]
fn test_struct_field_annotation_binds_project_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/app.rs",
            "pub struct St { pub x: i32 }\n\
             pub struct Holder<T> {\n    pub st_field: St,\n    pub plain: i32,\n    pub generic_field: T,\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "st_field"), [pair("src/app.rs", "St")]);
    assert_eq!(annotated(&g, "plain"), Vec::<(String, String)>::new());
    assert_eq!(
        annotated(&g, "generic_field"),
        Vec::<(String, String)>::new()
    );
}

/// Contract: a tuple-struct field has no name token, so it is a `Property`
/// named by its position, and it binds its type like a named field.
#[test]
fn test_tuple_struct_field_annotation_binds_project_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/app.rs",
            "pub struct St;\npub struct Pair(pub St, pub i32);\n",
        )],
    );
    assert_eq!(annotated(&g, "0"), [pair("src/app.rs", "St")]);
    assert_eq!(annotated(&g, "1"), Vec::<(String, String)>::new());
}

/// Contract: enum variant fields, struct-form and tuple-form, bind their
/// type; a unit variant has no field and no edge.
#[test]
fn test_enum_variant_field_annotation_binds_project_type() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[(
            "src/app.rs",
            "pub struct St;\npub struct Al;\n\
             pub enum E { V { st_var: St }, W(Al), Z }\n",
        )],
    );
    assert_eq!(annotated(&g, "st_var"), [pair("src/app.rs", "St")]);
    assert_eq!(annotated(&g, "0"), [pair("src/app.rs", "Al")]);
    assert_eq!(annotated(&g, "Z"), Vec::<(String, String)>::new());
}

/// Edge: an empty tuple struct `struct Unit();` emits no field, and the same
/// file parsed twice gives the same single edge.
#[test]
fn test_empty_tuple_struct_emits_no_field_edge() {
    let src = [(
        "src/app.rs",
        "pub struct St;\npub struct Unit();\npub struct Empty {}\n",
    )];
    let g = graph(&RustProvider::new().unwrap(), &src);
    assert_eq!(annotated(&g, "0"), Vec::<(String, String)>::new());
    let again = graph(&RustProvider::new().unwrap(), &src);
    assert_eq!(annotated(&again, "0"), annotated(&g, "0"));
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(_token: &str) -> String {
        "src/lib.rs".to_string()
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let files = vec![(
            token_file(token),
            format!(
                "pub struct {token};\npub struct Bag{params} {{\n    items: Vec<{arg}>,\n}}\npub struct Holder {{\n    tokens: Vec<{token}>,\n}}\n"
            ),
        )];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&RustProvider::new().unwrap(), &refs)
    }

    /// Stands for: a field whose type argument names a class type parameter
    /// binds no project type, although a project class has the same name; the
    /// sibling field in an unparameterised holder (POSITIVE CONTROL) binds it.
    #[test]
    fn test_field_annotation_class_type_parameter_named_like_project_class_binds_nothing() {
        let g = build(Some("Token"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "items binds a type parameter as a project class: {:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for: a generic argument in a holder with no type parameters
    /// binds the project class it names.
    #[test]
    fn test_field_annotation_project_class_in_unparameterised_holder_binds_class() {
        let g = build(Some("Token"), "Token");
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the parameter): `Box<E>` / `List<E>` binds no
    /// project edge, and `tokens` still binds `Token`.
    #[test]
    fn test_field_annotation_renamed_class_type_parameter_binds_nothing() {
        let g = build(Some("E"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the project class): the project class is `Tok`
    /// and the parameter stays `Token`; `items` binds neither, `tokens` binds `Tok`.
    #[test]
    fn test_field_annotation_renamed_project_class_keeps_parameter_binding_nothing() {
        let g = build(Some("Token"), "Tok");
        let items = names(&g, "items");
        assert!(
            !items.iter().any(|n| n == "Tok" || n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Tok"), "Tok")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, remove the parameter): with no class type parameter
    /// the same field binds the project class `Token`.
    #[test]
    fn test_field_annotation_without_class_type_parameter_binds_project_class() {
        let g = build(None, "Token");
        assert!(
            annotated(&g, "items").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "items")
        );
    }

    fn method_files() -> Vec<(String, String)> {
        vec![(
            token_file("Token"),
            "pub struct Token;\npub struct Bag;\nimpl Bag {\n    pub fn put<Token>(xs: Vec<Token>) {}\n}\npub struct Holder;\nimpl Holder {\n    pub fn take(xs: Vec<Token>) {}\n}\n".to_string(),
        )]
    }

    /// Stands for: a method type parameter named like a project class binds
    /// nothing for `put`; `take`, declared where no such parameter exists
    /// (POSITIVE CONTROL), binds the project class.
    #[test]
    fn test_field_annotation_method_type_parameter_named_like_project_class_binds_nothing() {
        let files = method_files();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        let g = graph(&RustProvider::new().unwrap(), &refs);
        assert!(
            !names(&g, "put").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "put")
        );
        assert!(
            annotated(&g, "take").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "take")
        );
    }
}
