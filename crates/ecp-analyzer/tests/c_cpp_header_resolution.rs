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
//!   the header prototype rather than a C++ overload. A vendor definition
//!   settles nothing either, and a vendor prototype never pushes a
//!   reachable definition out. A `static` function of another file, or one
//!   in an anonymous namespace, is no candidate at all.
//! - A K&R parameter no declaration types is an implicit `int`: such a
//!   definition settles its prototype. `f()` is the unspecified form and
//!   matches a definition with parameters; `f(void)` matches none.
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
use ecp_core::graph::{RelType, ZeroCopyGraph};
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
/// another file, so the caller keeps its edge to the prototype, the only
/// declaration it can reach (A06 sibling C1).
#[test]
fn test_c_family_field_cpp_caller_header_prototype_unreachable_static_binds_prototype() {
    let g = graph(&[
        ("api.h", "void sfn(void);\n"),
        ("a.c", "static void sfn(void) {}\n"),
        ("main.cpp", "#include \"api.h\"\nvoid run() { sfn(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("api.h", "sfn")]);
}

/// Stands for: a `static` definition in another file is no candidate at
/// all, so the prototype is the one declaration the C++ caller reaches and
/// it binds the prototype (A06 sibling C2; it was "binds nothing").
#[test]
fn test_c_family_field_cpp_caller_static_definition_in_other_file_binds_prototype() {
    let g = graph(&[
        ("types.hpp", "void sf();\n"),
        ("a.cpp", "static void sf() {}\n"),
        ("main.cpp", "#include \"types.hpp\"\nvoid run() { sf(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("types.hpp", "sf")]);
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

/// Stands for: C89 implicit `int` is accepted (`KR_IMPLICIT_INT = true`), so
/// a K&R parameter no declaration types is an `int` and its definition
/// settles the prototype (it was "settles nothing").
#[test]
fn test_c_family_field_cpp_caller_kr_definition_with_untyped_parameter_binds_definition() {
    let g = graph(&[
        ("api.h", "int kr2(int, int);\n"),
        ("lib.c", "int kr2(a, b) int a; { return a; }\n"),
        (
            "main.cpp",
            "#include \"api.h\"\nvoid run() { kr2(1, 2); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "kr2")]);
}

// ── A05: an unrelated class method beside the free function ─────────────

const A05_BASE: &[(&str, &str)] = &[
    ("api.h", "void lib_init(void);\n"),
    ("lib.c", "#include \"api.h\"\nvoid lib_init(void) {}\n"),
    (
        "main.cpp",
        "#include \"api.h\"\nvoid run(void) { lib_init(); }\n",
    ),
];
const A05_WIDGET: (&str, &str) = (
    "widget.cpp",
    "struct W { void lib_init(); };\nvoid W::lib_init() {}\n",
);

/// Stands for: a bare call is not a member call, so a method of an unrelated
/// class named like the free function neither takes the edge nor hides the
/// C definition from the C++ caller.
#[test]
fn test_c_family_field_cpp_caller_unrelated_class_method_namesake_binds_c_definition() {
    let mut files = A05_BASE.to_vec();
    files.push(A05_WIDGET);
    let g = graph(&files);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "lib_init")]);
    // Control: the project without `widget.cpp` binds the same edge.
    let g = graph(A05_BASE);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "lib_init")]);
}

/// Stands for: a member calling its own class's method binds that member,
/// never the same-named C function (the other half of the A05 pair).
#[test]
fn test_c_family_field_cpp_member_caller_own_method_binds_member() {
    let g = graph(&[
        ("api.h", "void lib_init(void);\n"),
        ("lib.c", "#include \"api.h\"\nvoid lib_init(void) {}\n"),
        (
            "widget.cpp",
            "struct W { void lib_init(); void go() { lib_init(); } };\nvoid W::lib_init() {}\n",
        ),
    ]);
    assert_eq!(calls(&g, "go"), vec![pair("widget.cpp", "lib_init")]);
}

/// Stands for (M02, C++ row): adding a same-named method in another class
/// does not change the free-function verdict of the caller.
#[test]
fn test_c_family_field_unrelated_class_method_added_keeps_free_function_verdict() {
    let base = graph(A05_BASE);
    let mut files = A05_BASE.to_vec();
    files.push(A05_WIDGET);
    let variant = graph(&files);
    assert_eq!(calls(&base, "run"), vec![pair("lib.c", "lib_init")]);
    assert_eq!(calls(&variant, "run"), calls(&base, "run"));
}

// ── A06: a `static` function of another file ────────────────────────────

fn static_user() -> (&'static str, &'static str) {
    ("m.c", "void run(void) { hid(); }\n")
}

/// Stands for: internal linkage keeps a function out of every other
/// translation unit, so the C caller binds nothing.
#[test]
fn test_c_family_field_c_caller_static_function_of_other_file_binds_nothing() {
    let g = graph(&[("a.c", "static void hid(void) {}\n"), static_user()]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

/// Stands for: the control of the `static` case: the same function without
/// `static` binds.
#[test]
fn test_c_family_field_c_caller_external_function_of_other_file_binds_it() {
    let g = graph(&[("a.c", "void hid(void) {}\n"), static_user()]);
    assert_eq!(calls(&g, "run"), vec![pair("a.c", "hid")]);
}

/// Stands for: a `static` function binds inside its own file (Tier 1).
#[test]
fn test_c_family_field_c_caller_same_file_static_binds_it() {
    let g = graph(&[(
        "m.c",
        "static void hid2(void) {}\nvoid run2(void) { hid2(); }\n",
    )]);
    assert_eq!(calls(&g, "run2"), vec![pair("m.c", "hid2")]);
}

/// Stands for: the `static` candidate leaves the field before the
/// uniqueness test, so the one external namesake is unique and binds.
#[test]
fn test_c_family_field_c_caller_static_namesake_of_external_binds_external() {
    let g = graph(&[
        ("a.c", "static void hid(void) {}\n"),
        ("b.c", "void hid(void) {}\n"),
        static_user(),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("b.c", "hid")]);
}

/// Stands for (M03): a `static` namesake added in another C file, or an
/// anonymous-namespace one in a C++ file, leaves the external target of the
/// caller as it was and takes no edge.
#[test]
fn test_c_family_field_c_caller_static_namesake_added_keeps_external_target() {
    let base = [("a.c", "void hid(void) {}\n"), static_user()];
    assert_eq!(calls(&graph(&base), "run"), vec![pair("a.c", "hid")]);
    let mut with_static = base.to_vec();
    with_static.push(("z.c", "static void hid(void) {}\n"));
    assert_eq!(calls(&graph(&with_static), "run"), vec![pair("a.c", "hid")]);
    let mut with_anonymous = base.to_vec();
    with_anonymous.push(("z.cpp", "namespace { void hid(void) {} }\n"));
    assert_eq!(
        calls(&graph(&with_anonymous), "run"),
        vec![pair("a.c", "hid")]
    );
}

/// Stands for: a function in an anonymous namespace has internal linkage
/// like a `static` one, so a C++ caller in another file binds nothing.
#[test]
fn test_c_family_field_cpp_caller_anonymous_namespace_function_of_other_file_binds_nothing() {
    let g = graph(&[
        ("a.cpp", "namespace { void anon_fn() {} }\n"),
        ("main.cpp", "void run() { anon_fn(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), Vec::<(String, String)>::new());
}

/// Stands for: the control of the anonymous-namespace case: the same
/// function outside the namespace binds.
#[test]
fn test_c_family_field_cpp_caller_plain_function_of_other_file_binds_it() {
    let g = graph(&[
        ("a.cpp", "void anon_fn() {}\n"),
        ("main.cpp", "void run() { anon_fn(); }\n"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("a.cpp", "anon_fn")]);
}

// ── A12: K&R undeclared parameter, `f()` and `f(void)` ──────────────────

/// The edges of `caller` to a function named `name`.
fn edges_named(g: &ZeroCopyGraph, caller: &str, name: &str) -> Vec<(String, String)> {
    calls(g, caller)
        .into_iter()
        .filter(|(_, callee)| callee == name)
        .collect()
}

/// Stands for: with implicit `int` accepted (`KR_IMPLICIT_INT = true`), a
/// K&R definition with an undeclared parameter is the one target of the
/// call. LOOSE bar: exactly one edge to a `kr`. STRICT bar: it lands in the
/// definition.
#[test]
fn test_c_family_field_c_caller_kr_definition_with_undeclared_parameter_binds_one_kr() {
    let g = graph(&[
        ("api.h", "int kr(int, int);\n"),
        ("lib.c", "int kr(a, b) int a; { return a; }\n"),
        (
            "main.c",
            "#include \"api.h\"\nint run(void) { return kr(1, 2); }\n",
        ),
    ]);
    assert_eq!(
        edges_named(&g, "run", "kr").len(),
        1,
        "{:?}",
        calls(&g, "run")
    );
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "kr")]);
}

const KR_LEGACY_HEADER: (&str, &str) = ("api.h", "int kr();\n");
const KR_LEGACY_DEFINITION: (&str, &str) =
    ("lib.c", "int kr(a, b) int a; int b; { return a + b; }\n");
const KR_LEGACY_USER: (&str, &str) = (
    "main.cpp",
    "#include \"api.h\"\nint prun() { return kr(1, 2); }\n",
);

/// Stands for: `int kr();` is the unspecified form, so the K&R definition
/// with two parameters is its definition and the C++ caller binds it, never
/// the header prototype.
#[test]
fn test_c_family_field_cpp_caller_legacy_header_kr_definition_binds_definition() {
    let g = graph(&[KR_LEGACY_HEADER, KR_LEGACY_DEFINITION, KR_LEGACY_USER]);
    assert_eq!(edges_named(&g, "prun", "kr").len(), 1);
    assert_eq!(calls(&g, "prun"), vec![pair("lib.c", "kr")]);
}

/// Stands for: a second legacy prototype of the same function in another
/// file is one more declaration of it, not another overload, so the caller
/// still binds the definition and never `other.c`'s prototype.
#[test]
fn test_c_family_field_cpp_caller_two_legacy_prototypes_kr_definition_binds_definition() {
    let g = graph(&[
        KR_LEGACY_HEADER,
        KR_LEGACY_DEFINITION,
        ("other.c", "int kr();\n"),
        KR_LEGACY_USER,
    ]);
    assert_eq!(edges_named(&g, "prun", "kr").len(), 1);
    assert_eq!(calls(&g, "prun"), vec![pair("lib.c", "kr")]);
}

const UNSPECIFIED_VS_VOID: &[(&str, &str)] = &[
    ("api.h", "int fv(void);\nint fu();\n"),
    (
        "lib.c",
        "int fv(int x) { return x; }\nint fu(int x) { return x; }\n",
    ),
    (
        "main.c",
        "#include \"api.h\"\nint run(void) { return fu(1); }\nint runv(void) { return fv(); }\n",
    ),
];

/// Stands for: `f()` declares no parameter list, so `fu(int)` is its
/// definition and the call binds it.
#[test]
fn test_c_family_field_c_caller_unspecified_form_matches_definition_with_parameters() {
    let g = graph(UNSPECIFIED_VS_VOID);
    assert_eq!(edges_named(&g, "run", "fu").len(), 1);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "fu")]);
}

/// Stands for: `f(void)` is a prototype with zero parameters, so `fv(int)`
/// is another function: the call binds the prototype, never that definition.
#[test]
fn test_c_family_field_c_caller_void_prototype_does_not_match_definition_with_parameters() {
    let g = graph(UNSPECIFIED_VS_VOID);
    assert_eq!(edges_named(&g, "runv", "fv").len(), 1);
    assert_eq!(calls(&g, "runv"), vec![pair("api.h", "fv")]);
}

/// Stands for: the control of the K&R cases: a fully typed K&R definition
/// binds for a C caller as an ANSI one does.
#[test]
fn test_c_family_field_c_caller_typed_kr_definition_binds_definition() {
    let g = graph(&[
        ("api.h", "int kr(int, int);\n"),
        ("lib.c", "int kr(a, b) int a; int b; { return a + b; }\n"),
        (
            "main.c",
            "#include \"api.h\"\nint run(void) { return kr(1, 2); }\n",
        ),
        ("ansi.h", "int ok(int);\n"),
        ("ansi.c", "int ok(int x) { return x; }\n"),
        (
            "use.c",
            "#include \"ansi.h\"\nint run2(void) { return ok(1); }\n",
        ),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "kr")]);
    assert_eq!(calls(&g, "run2"), vec![pair("ansi.c", "ok")]);
}

// ── A15e: a C file's own typedef is no import ───────────────────────────

/// `(source file, target file)` of each `Imports` edge whose target is named
/// `name`.
fn imports_of(g: &ZeroCopyGraph, name: &str) -> Vec<(String, String)> {
    let pool = g.string_pool.as_slice();
    let file_of = |idx: u32| {
        let node = &g.nodes[idx as usize];
        if node.has_owning_file() {
            g.files[node.file_idx as usize]
                .path
                .resolve(pool)
                .to_string()
        } else {
            String::new()
        }
    };
    let mut out: Vec<(String, String)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Imports)
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == name)
        .map(|e| (file_of(e.source), file_of(e.target)))
        .collect();
    out.sort();
    out
}

/// Stands for: a file never imports a typedef it declares itself (it was
/// `Imports main.c -> main.c::cb_t`), while a typedef a header declares still
/// reaches its includer through the type annotation.
#[test]
fn test_post_process_imports_c_self_declared_typedef_emits_no_imports_edge() {
    let g = graph(&[
        (
            "main.c",
            "typedef void (*cb_t)(int *);\nvoid apply(cb_t f) {}\n",
        ),
        ("t.h", "typedef struct { int a; } Al;\n"),
        (
            "user.c",
            "#include \"t.h\"\nAl al_ret(void) { Al a; return a; }\n",
        ),
    ]);
    assert_eq!(imports_of(&g, "cb_t"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "al_ret"), vec![pair("t.h", "Al")]);
}

/// Stands for: the control of the typedef case: a C caller still binds the
/// typedef its header declares.
#[test]
fn test_c_family_field_c_caller_included_typedef_still_binds_header_typedef() {
    let g = graph(&[
        ("t.h", "typedef struct { int a; } Al;\n"),
        (
            "user.c",
            "#include \"t.h\"\nAl al_ret(void) { Al a; return a; }\n",
        ),
    ]);
    assert_eq!(annotated(&g, "al_ret"), vec![pair("t.h", "Al")]);
}

// ── A15f: a qualified function value ────────────────────────────────────

const QUALIFIED_FILES: &[(&str, &str)] = &[
    (
        "types.hpp",
        "#pragma once\nnamespace ns {\nvoid cb_qual(int &x);\nstruct Inst { void cb_ptr(int &x); static void cb_ns(int &x); };\n}\nvoid cb_bare(int &x);\n",
    ),
    (
        "cb.cpp",
        "#include \"types.hpp\"\nvoid cb_bare(int &x) {}\nvoid ns::cb_qual(int &x) {}\nvoid ns::Inst::cb_ptr(int &x) {}\nvoid ns::Inst::cb_ns(int &x) {}\n",
    ),
    (
        "main.cpp",
        "#include \"types.hpp\"\n#include <algorithm>\n#include <vector>\n\
         void use_bare(std::vector<int> a) { std::for_each(a.begin(), a.end(), cb_bare); }\n\
         void use_qual(std::vector<int> a) { std::for_each(a.begin(), a.end(), &ns::cb_qual); }\n\
         void use_qual2(std::vector<int> a) { std::for_each(a.begin(), a.end(), ns::cb_qual); }\n\
         void use_ns(std::vector<int> a) { std::for_each(a.begin(), a.end(), &ns::Inst::cb_ns); }\n\
         void call_qual(std::vector<int> a) { int v = 0; ns::cb_qual(v); }\n",
    ),
];

/// Stands for: a qualified function value binds the definition a header
/// declaration is settled to, as the bare value does, never the header
/// declaration.
#[test]
fn test_c_family_field_cpp_qualified_function_value_binds_definition_not_prototype() {
    let g = graph(QUALIFIED_FILES);
    for registrar in ["use_qual", "use_qual2"] {
        assert_one_cross_file(&function_refs(&g, registrar), "cb.cpp", "cb_qual");
    }
}

/// Stands for: a qualified static member value binds its out-of-line
/// definition, not the declaration inside the class in the header.
#[test]
fn test_c_family_field_cpp_qualified_static_member_value_binds_definition() {
    let g = graph(QUALIFIED_FILES);
    assert_one_cross_file(&function_refs(&g, "use_ns"), "cb.cpp", "cb_ns");
}

/// Stands for (M09, C++ rows): a qualified call and a qualified value reach
/// one canonical declaration, and so do a bare call and a bare value.
#[test]
fn test_c_family_field_cpp_qualified_call_and_value_bind_same_declaration() {
    let g = graph(QUALIFIED_FILES);
    assert_eq!(calls(&g, "call_qual"), vec![pair("cb.cpp", "cb_qual")]);
    let value = function_refs(&g, "use_qual");
    assert_eq!(value.len(), 1, "{value:?}");
    assert_eq!(
        (value[0].0.as_str(), value[0].1.as_str()),
        ("cb.cpp", "cb_qual")
    );
}

/// Stands for: the control of the qualified case: the bare value already
/// binds the definition.
#[test]
fn test_c_family_field_cpp_bare_function_value_binds_definition() {
    let g = graph(QUALIFIED_FILES);
    assert_one_cross_file(&function_refs(&g, "use_bare"), "cb.cpp", "cb_bare");
}

// ── A15g: a C++ namespace typedef or alias ──────────────────────────────

const NAMESPACE_ALIAS_FILES: &[(&str, &str)] = &[
    (
        "types.hpp",
        "#include <utility>\nnamespace ns {\nstruct St { int x; };\nusing Al = std::pair<int,int>;\ntypedef std::pair<int,int> AlT;\n}\ntypedef int Gint;\n",
    ),
    (
        "m.c",
        "#include \"types.hpp\"\nAl al_ret(void) { Al a; return a; }\nAlT alt_ret(void) { AlT a; return a; }\nGint g_ret(void) { return 0; }\n",
    ),
    (
        "m2.cpp",
        "#include \"types.hpp\"\nns::Al al2(void) { return ns::Al(); }\n",
    ),
];

/// Stands for: C never binds a namespaced declaration, a `using` alias and
/// a `typedef` inside `namespace ns` included.
#[test]
fn test_c_family_field_c_caller_cpp_namespace_typedef_binds_nothing() {
    let g = graph(NAMESPACE_ALIAS_FILES);
    assert_eq!(annotated(&g, "al_ret"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "alt_ret"), Vec::<(String, String)>::new());
}

/// Stands for: the control of the namespace case: an owner-less typedef of
/// the same header binds.
#[test]
fn test_c_family_field_c_caller_global_typedef_binds_header_typedef() {
    let g = graph(NAMESPACE_ALIAS_FILES);
    assert_eq!(annotated(&g, "g_ret"), vec![pair("types.hpp", "Gint")]);
}

/// Stands for: the qualified C++ reference to the namespace alias keeps its
/// edge.
#[test]
fn test_c_family_field_cpp_caller_qualified_namespace_alias_binds_alias() {
    let g = graph(NAMESPACE_ALIAS_FILES);
    assert_eq!(annotated(&g, "al2"), vec![pair("types.hpp", "Al")]);
}

// ── A15h: `extern` on a definition with a body ──────────────────────────

const EXTERN_HEADER: (&str, &str) = ("api.h", "int ext(int);\n");

fn extern_user(file: &'static str) -> (&'static str, &'static str) {
    (
        file,
        "#include \"api.h\"\nint run(void) { return ext(1); }\n",
    )
}

/// Stands for: a definition with a body is the definition whatever its
/// storage class, so the C++ caller binds it and never the prototype.
#[test]
fn test_c_family_field_cpp_caller_extern_definition_binds_definition() {
    let g = graph(&[
        EXTERN_HEADER,
        (
            "lib.c",
            "#include \"api.h\"\nextern int ext(int x) { return x; }\n",
        ),
        extern_user("main.cpp"),
    ]);
    assert_eq!(calls(&g, "run"), vec![pair("lib.c", "ext")]);
}

/// Stands for (M09c): the `extern` keyword on a definition changes no
/// `Calls` row, for a C++ caller and a C caller alike.
#[test]
fn test_c_family_field_extern_keyword_does_not_change_calls() {
    for caller in ["main.cpp", "main.c"] {
        let with_extern = graph(&[
            EXTERN_HEADER,
            (
                "lib.c",
                "#include \"api.h\"\nextern int ext(int x) { return x; }\n",
            ),
            extern_user(caller),
        ]);
        let without = graph(&[
            EXTERN_HEADER,
            (
                "lib.c",
                "#include \"api.h\"\nint ext(int x) { return x; }\n",
            ),
            extern_user(caller),
        ]);
        assert_eq!(
            calls(&without, "run"),
            vec![pair("lib.c", "ext")],
            "{caller}"
        );
        assert_eq!(
            calls(&with_extern, "run"),
            calls(&without, "run"),
            "{caller}"
        );
    }
}

// ── M09: a call and a value reach one declaration ───────────────────────

/// Stands for (M09, C row): in C a call and a value of one function land on
/// one canonical declaration, the definition behind its header prototype.
#[test]
fn test_c_family_field_c_function_call_and_value_bind_same_declaration() {
    let g = graph(&[
        ("types.h", "void cb_bare(int *x);\n"),
        ("cb.c", "#include \"types.h\"\nvoid cb_bare(int *x) {}\n"),
        (
            "main.c",
            "#include \"types.h\"\ntypedef void (*cb_t)(int *);\nvoid apply(cb_t f) {}\nvoid use_bare(void) { apply(cb_bare); }\nvoid run(void) { int v; cb_bare(&v); }\n",
        ),
    ]);
    let value = function_refs(&g, "use_bare");
    assert_eq!(value.len(), 1, "{value:?}");
    let call = calls(&g, "run");
    assert_eq!(call, vec![pair("cb.c", "cb_bare")]);
    assert_eq!(
        (value[0].0.clone(), value[0].1.clone()),
        call[0].clone(),
        "one canonical declaration"
    );
}
