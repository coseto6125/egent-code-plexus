//! C and C++ names declared in a header (`ecp-core` `analyzer/c_family.rs`).
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
//! - A prototype and one definition are one target, the definition, when
//!   both have the same owner and parameter types (names dropped, spelling
//!   normalised) and the caller can reach the definition. Two definitions
//!   stay ambiguous: no edge.
//! - A definition of another parameter count or other parameter types is
//!   another overload: it never settles the prototype, and a C caller binds
//!   the header prototype rather than a C++ overload. A vendor definition,
//!   or a `static` one in another file, settles nothing either, and a
//!   vendor prototype never pushes a reachable definition out.
//! - A definition whose parameter types the meta cannot read (a K&R name
//!   no declaration types) settles nothing.
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

// ── What settles a prototype ────────────────────────────────────────────

/// Stands for: a definition of another parameter count is no definition of
/// the prototype, so the C++ caller gets no edge to that overload.
#[test]
fn test_c_family_field_cpp_caller_overload_of_other_arity_binds_nothing() {
    let g = graph(&[
        ("api.h", "void lib_init(void);\n"),
        ("lib.c", "#include \"api.h\"\nvoid lib_init(void) {}\n"),
        ("other.cpp", "void lib_init(int mode) {}\n"),
        (
            "main.cpp",
            "#include \"api.h\"\nvoid run(void) { lib_init(); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

/// Stands for: a C caller binds the prototype its header declares, never a
/// C++ overload of another parameter count.
#[test]
fn test_c_family_field_c_caller_cpp_overload_of_other_arity_binds_prototype() {
    let g = graph(&[
        ("api.h", "void ext_init(void);\n"),
        ("other.cpp", "void ext_init(int mode) {}\n"),
        (
            "m.c",
            "#include \"api.h\"\nvoid crun(void) { ext_init(); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "crun"), vec![pair("api.h", "ext_init")]);
}

/// Stands for: a vendor definition settles no prototype for a non-vendor
/// caller, so the caller keeps its edge to the prototype.
#[test]
fn test_c_family_field_c_caller_vendor_definition_binds_prototype() {
    let g = graph(&[
        ("api.h", "void ext_fn(void);\n"),
        (
            "src/main.c",
            "#include \"../api.h\"\nvoid run(void) { ext_fn(); }\n",
        ),
        ("vendor/unused/impl.cpp", "void ext_fn(void) {}\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("api.h", "ext_fn")]);
}

/// Stands for: a `static` C function never crosses to a C++ caller in
/// another file, so the caller keeps its edge to the prototype.
#[test]
fn test_c_family_field_cpp_caller_static_c_definition_binds_prototype() {
    let g = graph(&[
        ("api.h", "void sfn(void);\n"),
        ("a.c", "static void sfn(void) {}\n"),
        ("main.cpp", "#include \"api.h\"\nvoid run() { sfn(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("api.h", "sfn")]);
}

/// Stands for: a `static` definition in another file settles no prototype,
/// so the prototype and that definition stay ambiguous.
#[test]
fn test_c_family_field_cpp_caller_static_definition_in_other_file_binds_nothing() {
    let g = graph(&[
        ("types.hpp", "void sf();\n"),
        ("a.cpp", "static void sf() {}\n"),
        ("main.cpp", "#include \"types.hpp\"\nvoid run() { sf(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

/// Stands for: the parameter count ignores names, so an unnamed-parameter
/// prototype is settled by its named definition.
#[test]
fn test_c_family_field_cpp_unnamed_parameter_prototype_binds_definition() {
    let g = graph(&[
        ("api.hpp", "void put(int, char *);\n"),
        ("put.cpp", "void put(int n, char *s) {}\n"),
        (
            "main.cpp",
            "#include \"api.hpp\"\nvoid run() { put(1, nullptr); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("put.cpp", "put")]);
}

/// Stands for: a variadic prototype and its definition count the `...`
/// alike, so the definition settles the prototype.
#[test]
fn test_c_family_field_c_caller_variadic_prototype_binds_definition() {
    let g = graph(&[
        ("log.h", "int log_msg(const char *fmt, ...);\n"),
        (
            "log.cpp",
            "int log_msg(const char *fmt, ...) { return 0; }\n",
        ),
        (
            "m.c",
            "#include \"log.h\"\nvoid run(void) { log_msg(\"x\", 1); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("log.cpp", "log_msg")]);
}

/// Stands for: one parameter count is not one function; `f(double)` is
/// another overload of the `f(int)` the header declares, so the C++ caller
/// gets no edge rather than one to the wrong overload.
#[test]
fn test_c_family_field_cpp_overload_of_same_arity_other_types_binds_nothing() {
    let g = graph(&[
        ("api.hpp", "void f(int);\n"),
        ("other.cpp", "void f(double) {}\n"),
        ("main.cpp", "#include \"api.hpp\"\nvoid run() { f(1); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

/// Stands for: a pointer or reference is part of the parameter type, so
/// `f(int *)` and `f(int &)` are other overloads of the `f(int)` the header
/// declares. A name anywhere in a declarator, and an array the language
/// adjusts to a pointer, leave the type as it is.
#[test]
fn test_c_family_field_cpp_overload_of_pointer_parameter_binds_nothing() {
    for definition in [
        "void f(int *p) {}\n",
        "void f(int &r) {}\n",
        "void f(int (*)[2]) {}\n",
    ] {
        let g = graph(&[
            ("api.hpp", "void f(int);\n"),
            ("other.cpp", definition),
            ("main.cpp", "#include \"api.hpp\"\nvoid run() { f(1); }\n"),
        ]);
        assert_eq!(
            calls(&g, "run"),
            Vec::<(String, String)>::new(),
            "{definition}"
        );
    }
    let g = graph(&[
        ("api.hpp", "void k(int a[4], char **, int (*)(int));\n"),
        (
            "k.cpp",
            "#include \"api.hpp\"\nvoid k(int *p, char ** argv, int (*cb)(int value)) {}\n",
        ),
        (
            "main.cpp",
            "#include \"api.hpp\"\nvoid run() { k(0, 0, 0); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("k.cpp", "k")]);
}

/// Stands for: a C caller binds the prototype its header declares, never a
/// C++ overload of the same count and other types.
#[test]
fn test_c_family_field_c_caller_cpp_overload_of_same_arity_other_types_binds_prototype() {
    let g = graph(&[
        ("api.h", "void g(int);\n"),
        ("other.cpp", "void g(double d) {}\n"),
        ("m.c", "#include \"api.h\"\nvoid crun(void) { g(1); }\n"),
    ]);
    assert_eq!(calls(&g, "crun"), vec![pair("api.h", "g")]);
}

/// Stands for: the type comparison drops names and reads one spelling, so
/// whitespace and a C elaborated `struct` keyword do not make the
/// definition another overload.
#[test]
fn test_c_family_field_cpp_prototype_of_other_type_spelling_binds_definition() {
    let g = graph(&[
        ("api.hpp", "void h(unsigned   int, struct node *);\n"),
        (
            "h.cpp",
            "#include \"api.hpp\"\nvoid h(unsigned int n, node *p) {}\n",
        ),
        (
            "main.cpp",
            "#include \"api.hpp\"\nvoid run() { h(1, 0); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("h.cpp", "h")]);
}

/// Stands for: a line break after the elaborated keyword is whitespace like
/// a space, so `struct\nFoo` and `Foo` are one parameter type.
#[test]
fn test_c_family_field_cpp_prototype_with_line_break_after_struct_binds_definition() {
    let g = graph(&[
        ("api.hpp", "struct Foo {};\nvoid f(struct\nFoo);\n"),
        ("impl.cpp", "#include \"api.hpp\"\nvoid f(Foo x) {}\n"),
        (
            "main.cpp",
            "#include \"api.hpp\"\nvoid run() { f(Foo{}); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("impl.cpp", "f")]);
}

/// Stands for: a `...` counts as a parameter on both sides of the family, so
/// a C++-parsed `.h` variadic prototype and its C definition are one
/// function, and a C definition without the `...` is another overload.
#[test]
fn test_c_family_field_cpp_caller_variadic_header_prototype_c_definition_binds_definition() {
    let header = ("api.h", "int lg(const char *fmt, ...);\n");
    let user = (
        "main.cpp",
        "#include \"api.h\"\nvoid prun() { lg(\"x\"); }\n",
    );
    let g = graph(&[
        header,
        ("lib.c", "int lg(const char *fmt, ...) { return 0; }\n"),
        user,
    ]);
    assert_eq!(calls(&g, "prun"), vec![pair("lib.c", "lg")]);
    let g = graph(&[
        header,
        ("lib.c", "int lg(const char *fmt) { return 0; }\n"),
        user,
    ]);
    assert_eq!(calls(&g, "prun"), vec![pair("api.h", "lg")]);
}

/// Stands for: a vendor prototype the caller cannot reach shapes nothing,
/// so the header definition of another count stays the caller's target.
#[test]
fn test_c_family_field_c_caller_vendor_prototype_beside_header_definition_binds_definition() {
    let g = graph(&[
        ("api.h", "void f(void) {}\n"),
        ("vendor/unused/api.hpp", "void f(int);\n"),
        ("main.c", "#include \"api.h\"\nvoid run(void) { f(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("api.h", "f")]);
}

/// Stands for: an unnamed `void *` is a parameter, not the `(void)` list,
/// so the header prototype and its named definition count alike, whichever
/// language defines it.
#[test]
fn test_c_family_field_cpp_unnamed_void_pointer_prototype_binds_definition() {
    for (file, definition) in [
        ("lib.cpp", "#include \"api.h\"\nvoid release(void *p) {}\n"),
        ("lib.c", "#include \"api.h\"\nvoid release(void *p) {}\n"),
    ] {
        let g = graph(&[
            ("api.h", "void release(void *);\n"),
            (file, definition),
            (
                "main.cpp",
                "#include \"api.h\"\nvoid run() { release(0); }\n",
            ),
        ]);
        assert_eq!(calls(&g, "run"), vec![pair(file, "release")], "{file}");
    }
}

/// Stands for: a K&R definition counts and types its identifier list from
/// the declarations before its body.
#[test]
fn test_c_family_field_cpp_caller_kr_definition_binds_definition() {
    let g = graph(&[
        ("api.h", "int kr(int, int);\n"),
        ("lib.c", "int kr(a, b) int a; int b; { return a; }\n"),
        ("main.cpp", "#include \"api.h\"\nvoid run() { kr(1, 2); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "kr")]);
}

/// Stands for: a K&R parameter no declaration types has an unknown type,
/// so its definition settles nothing and stays beside the prototype.
#[test]
fn test_c_family_field_cpp_caller_kr_definition_with_untyped_parameter_binds_nothing() {
    let g = graph(&[
        ("api.h", "int kr2(int, int);\n"),
        ("lib.c", "int kr2(a, b) int a; { return a; }\n"),
        (
            "main.cpp",
            "#include \"api.h\"\nvoid run() { kr2(1, 2); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}
