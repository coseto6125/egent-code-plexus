//! C and C++ names declared in a header (`resolution/c_family.rs`).
//!
//! A `.h` file is C++ by extension, so before the family rule a C caller
//! bound none of the types and functions its headers declare. A C++
//! function declared in a header and defined in a `.cpp` had two
//! candidates, the prototype and the definition, and bound neither.
//!
//! Each fixture parses a path with the provider the router gives it: `.c`
//! with C, `.h` / `.hpp` / `.cpp` with C++, `.rs` with Rust.
//!
//! Contracts:
//! - A C caller binds an owner-less struct, typedef or function a `.h`
//!   declares; a C++ caller binds the C definition of a function its header
//!   declares.
//! - A prototype and one definition are one target, the definition. Two
//!   definitions stay ambiguous: no edge.
//! - An own-language definition wins over the other family language's.
//! - C never binds a C++-only declaration: one inside a namespace, or a
//!   class. No other language pair crosses: Rust binds no C function.
//! - The vendor barrier still holds across the family.
use crate::name_ref_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::ZeroCopyGraph;
use name_ref_support::{annotated, assert_one_cross_file, calls, function_refs};
use std::path::Path;

/// The graph of `files`, each parsed by the provider of its extension.
fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let c = CProvider::new().expect("CProvider init");
    let cpp = CppProvider::new().expect("CppProvider init");
    let rust = RustProvider::new().expect("RustProvider init");
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        let provider: &dyn LanguageProvider =
            match Path::new(path).extension().and_then(|e| e.to_str()) {
                Some("c") => &c,
                Some("rs") => &rust,
                _ => &cpp,
            };
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

// ── C caller, declaration in a `.h` ─────────────────────────────────────

#[test]
fn test_c_family_field_c_caller_header_struct_and_typedef_bind_header() {
    let g = graph(&[
        (
            "t.h",
            "struct St { int x; };\ntypedef struct { int a; } Al;\n",
        ),
        (
            "m.c",
            "#include \"t.h\"\n\
             struct St st_ret(void) { struct St s; return s; }\n\
             Al al_ret(void) { Al a; return a; }\n",
        ),
    ]);
    assert_eq!(annotated(&g, "st_ret"), vec![pair("t.h", "St")]);
    assert_eq!(annotated(&g, "al_ret"), vec![pair("t.h", "Al")]);
}

#[test]
fn test_c_family_field_c_caller_header_prototype_c_definition_binds_definition() {
    let g = graph(&[
        ("types.h", "void cb_bare(int *x);\n"),
        ("cb.c", "#include \"types.h\"\nvoid cb_bare(int *x) {}\n"),
        (
            "main.c",
            "#include \"types.h\"\n\
             typedef void (*cb_t)(int *);\n\
             void apply(cb_t f) {}\n\
             void use_bare(void) { apply(cb_bare); }\n\
             void run(void) { int v; cb_bare(&v); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("cb.c", "cb_bare")]);
    assert_one_cross_file(&function_refs(&g, "use_bare"), "cb.c", "cb_bare");
}

#[test]
fn test_c_family_field_c_caller_header_prototype_cpp_definition_binds_definition() {
    let g = graph(&[
        ("api.h", "void lib_init(void);\n"),
        ("lib.cpp", "#include \"api.h\"\nvoid lib_init(void) {}\n"),
        (
            "m.c",
            "#include \"api.h\"\nvoid run(void) { lib_init(); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("lib.cpp", "lib_init")]);
}

#[test]
fn test_c_family_field_c_caller_prototype_without_definition_binds_prototype() {
    let g = graph(&[
        ("api.h", "void ext_fn(void);\n"),
        ("m.c", "#include \"api.h\"\nvoid run(void) { ext_fn(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("api.h", "ext_fn")]);
}

#[test]
fn test_c_family_field_c_caller_namespace_member_binds_nothing() {
    let g = graph(&[
        (
            "t.h",
            "namespace ns { void helper(void); struct Box { int x; }; }\n",
        ),
        (
            "m.c",
            "#include \"t.h\"\n\
             void run(void) { helper(); }\n\
             struct Box box_ret(void) { struct Box b; return b; }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "box_ret"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_c_caller_cpp_class_binds_nothing() {
    let g = graph(&[
        ("t.h", "class Widget { public: int x; };\n"),
        (
            "m.c",
            "#include \"t.h\"\nWidget w_ret(void) { Widget w; return w; }\n",
        ),
    ]);
    assert_eq!(annotated(&g, "w_ret"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_same_name_in_c_and_cpp_binds_own_language() {
    let g = graph(&[
        ("a.c", "int helper(void) { return 0; }\n"),
        ("b.cpp", "int helper() { return 1; }\n"),
        ("m.c", "int run_c(void) { return helper(); }\n"),
        ("n.cpp", "int run_cpp() { return helper(); }\n"),
    ]);
    assert_eq!(calls(&g, "run_c"), vec![pair("a.c", "helper")]);
    assert_eq!(calls(&g, "run_cpp"), vec![pair("b.cpp", "helper")]);
}

#[test]
fn test_c_family_field_vendor_header_for_source_caller_binds_nothing() {
    let g = graph(&[
        ("vendor/x/t.h", "struct St { int x; };\n"),
        (
            "src/m.c",
            "struct St st_ret(void) { struct St s; return s; }\n",
        ),
    ]);
    assert_eq!(annotated(&g, "st_ret"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_rust_caller_c_function_binds_nothing() {
    let g = graph(&[
        ("lib.c", "void c_only(void) {}\n"),
        ("main.rs", "fn run() { c_only(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_empty_header_c_caller_binds_nothing() {
    let g = graph(&[
        ("t.h", ""),
        ("m.c", "#include \"t.h\"\nvoid run(void) { missing(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

// ── C++ caller, declaration in a header ─────────────────────────────────

#[test]
fn test_c_family_field_cpp_header_prototype_and_definition_bind_definition() {
    let g = graph(&[
        ("types.hpp", "void cb_bare(int &x);\n"),
        (
            "cb.cpp",
            "#include \"types.hpp\"\nvoid cb_bare(int &x) {}\n",
        ),
        (
            "main.cpp",
            "#include \"types.hpp\"\n\
             #include <algorithm>\n\
             #include <vector>\n\
             void use_bare(std::vector<int> a) { std::for_each(a.begin(), a.end(), cb_bare); }\n\
             void run() { int v = 0; cb_bare(v); cb_bare(v); }\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "use_bare"), "cb.cpp", "cb_bare");
    // One edge per call site, as for any callee: the second site picks the
    // same definition, not the prototype.
    assert_eq!(
        calls(&g, "run"),
        vec![pair("cb.cpp", "cb_bare"), pair("cb.cpp", "cb_bare")]
    );
}

#[test]
fn test_c_family_field_cpp_two_definitions_and_prototype_bind_nothing() {
    let g = graph(&[
        ("types.hpp", "void dup(int &x);\n"),
        ("a.cpp", "void dup(int &x) {}\n"),
        ("b.cpp", "void dup(int &x) {}\n"),
        (
            "main.cpp",
            "#include \"types.hpp\"\nvoid run() { int v = 0; dup(v); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_cpp_prototype_and_definition_of_other_owner_bind_nothing() {
    let g = graph(&[
        ("types.hpp", "void g();\n"),
        ("a.cpp", "namespace ns { void g() {} }\n"),
        ("main.cpp", "#include \"types.hpp\"\nvoid run() { g(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

#[test]
fn test_c_family_field_cpp_two_prototypes_one_definition_bind_definition() {
    let g = graph(&[
        ("a.hpp", "void twice(int &x);\n"),
        ("b.hpp", "void twice(int &x);\n"),
        ("impl.cpp", "void twice(int &x) {}\n"),
        (
            "main.cpp",
            "#include \"a.hpp\"\n#include \"b.hpp\"\nvoid run() { int v = 0; twice(v); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("impl.cpp", "twice")]);
}

#[test]
fn test_c_family_field_cpp_prototype_without_definition_binds_prototype() {
    let g = graph(&[
        ("t.hpp", "void ext();\n"),
        ("main.cpp", "#include \"t.hpp\"\nvoid run() { ext(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("t.hpp", "ext")]);
}

#[test]
fn test_c_family_field_cpp_caller_header_prototype_c_definition_binds_definition() {
    let g = graph(&[
        ("api.h", "void c_impl(void);\n"),
        ("impl.c", "void c_impl(void) {}\n"),
        ("main.cpp", "#include \"api.h\"\nvoid run() { c_impl(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("impl.c", "c_impl")]);
}
