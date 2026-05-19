//! `.h` header guards (`#ifndef X / #define X / ... / #endif`) parse as
//! a `preproc_ifdef` intermediate node in tree-sitter-cpp, so the previous
//! `(translation_unit (declaration ...))` anchor missed every prototype
//! sitting inside the guard — which is *every* prototype in *every*
//! real `.h` file. Regression for the Round 65 surfacing on hiredis
//! `dict.h` (10 static prototypes → 0 captured before the anchor drop).

use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CppProvider::new().expect("CppProvider init");
    p.parse_file(Path::new("t.h"), src.as_bytes()).expect("parse_file")
}

fn fns(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn header_guard_wrapped_prototypes_emit_function() {
    let src = "#ifndef DICT_H\n#define DICT_H\nvoid foo(int x);\nint bar(void);\n#endif\n";
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"foo"), "expected foo in {names:?}");
    assert!(names.contains(&"bar"), "expected bar in {names:?}");
}

#[test]
fn static_prototype_inside_header_guard_emits_function() {
    // Mirrors hiredis dict.h pattern — static-qualified prototypes at module
    // scope behind the standard #ifndef header guard.
    let src = r#"
#ifndef DICT_H
#define DICT_H

typedef struct dict { int v; } dict;

static int dictAdd(dict *d, void *key);
static void dictRelease(dict *d);

#endif
"#;
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"dictAdd"), "expected dictAdd in {names:?}");
    assert!(names.contains(&"dictRelease"), "expected dictRelease in {names:?}");
}

#[test]
fn nested_ifdef_inside_extern_c_still_emits() {
    // Real headers (hdr_histogram etc.) often nest a C++-compat extern "C"
    // inside the outer header guard.
    let src = r#"
#ifndef HDR_H
#define HDR_H

#ifdef __cplusplus
extern "C" {
#endif

int hdr_init(int sig_figs);
void hdr_close(void *h);

#ifdef __cplusplus
}
#endif

#endif
"#;
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"hdr_init"), "expected hdr_init in {names:?}");
    assert!(names.contains(&"hdr_close"), "expected hdr_close in {names:?}");
}

#[test]
fn class_method_declaration_promotes_to_method_not_function() {
    // Critical regression: without the translation_unit anchor, anything
    // inside `field_declaration_list` (class body) could leak as Function.
    // The parser's `is_inline_class_member` walker must promote correctly.
    let src = r#"
class Foo {
public:
    void bar(int x);
    int baz() const;
};
"#;
    let g = parse(src);
    // No Function emissions for class members.
    let fn_names = fns(&g);
    assert!(
        !fn_names.contains(&"bar"),
        "class member `bar` must not emit as Function; nodes: {fn_names:?}"
    );
    assert!(
        !fn_names.contains(&"baz"),
        "class member `baz` must not emit as Function; nodes: {fn_names:?}"
    );
    // They should emit as Method instead.
    let methods: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.name.as_str())
        .collect();
    assert!(methods.contains(&"bar"), "expected `bar` as Method in {methods:?}");
    assert!(methods.contains(&"baz"), "expected `baz` as Method in {methods:?}");
}

#[test]
fn namespace_scoped_prototype_stays_function() {
    // Free functions inside a namespace must keep Function classification,
    // not get promoted to Method. The walker stops at namespace_definition.
    let src = "namespace foo {\n    int bar(int x);\n    void baz();\n}\n";
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"bar"), "expected `bar` as Function in {names:?}");
    assert!(names.contains(&"baz"), "expected `baz` as Function in {names:?}");
}
