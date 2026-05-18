//! C function prototypes wrapped in `extern "C" { ... }` must emit as
//! Function. Pure-C library headers (hdr_histogram, fpconv, libuv, redis
//! internals) routinely wrap every declaration in `extern "C"` for C++
//! interop. tree-sitter-cpp parses that as
//! `linkage_specification > declaration_list > declaration`, so the
//! translation_unit-anchored free-function rule misses every prototype
//! inside the wrapper.
//!
//! Regression for the gap surfaced by Round 64 — Cpp Function 2003
//! candidates after the `.h → Cpp` dispatch fix landed.

use graph_nexus_analyzer::cpp::parser::CppProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
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
fn extern_c_block_emits_each_prototype() {
    let src = r#"
#ifdef __cplusplus
extern "C" {
#endif

int hdr_add(int a, int b);
int hdr_sub(int a, int b);
void hdr_close(void* h);

#ifdef __cplusplus
}
#endif
"#;
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"hdr_add"), "expected hdr_add in {names:?}");
    assert!(names.contains(&"hdr_sub"), "expected hdr_sub in {names:?}");
    assert!(names.contains(&"hdr_close"), "expected hdr_close in {names:?}");
}

#[test]
fn extern_c_single_decl_emits_function() {
    let src = "extern \"C\" int single_decl(int x);\n";
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"single_decl"), "expected single_decl in {names:?}");
}

#[test]
fn extern_c_pointer_return_emits_function() {
    let src = r#"
extern "C" {
char* dup_str(const char* s);
void* alloc_ptr(int n);
}
"#;
    let g = parse(src);
    let names = fns(&g);
    assert!(names.contains(&"dup_str"), "expected dup_str in {names:?}");
    assert!(names.contains(&"alloc_ptr"), "expected alloc_ptr in {names:?}");
}

#[test]
fn plain_translation_unit_function_still_emits() {
    // Guard against regressing the unchanged translation_unit-level path.
    let src = "int outside_extern(int x);\n";
    let g = parse(src);
    assert!(fns(&g).contains(&"outside_extern"));
}

#[test]
fn extern_c_with_function_definition_emits_via_existing_rule() {
    // function_definition (with body) under extern "C" — already captured by
    // the bodied function_definition rule because that rule isn't anchored to
    // translation_unit. Pin the behaviour so the new linkage_specification
    // rules don't introduce a double-emit.
    let src = r#"
extern "C" {
int with_body(int x) { return x + 1; }
}
"#;
    let g = parse(src);
    let count = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name == "with_body")
        .count();
    assert_eq!(count, 1, "with_body must emit exactly once; nodes: {:#?}", g.nodes);
}
