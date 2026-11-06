//! C "Named binding" dimension: `#define` (object-like and function-like)
//! and `extern` declarations surface as `RawImport` entries (a `typedef` is
//! a `Typedef` node, CONTRACTS 8 / A15e), each with `alias = Some(short_name)`
//! so downstream resolvers can do qualifier-scope lookups the same way they do for Java static imports.
//!
//! Reference implementation: Java in `src/java/parser.rs`.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawImport> {
    let provider = CProvider::new().expect("CProvider init");
    let graph = provider
        .parse_file(Path::new("t.c"), src.as_bytes())
        .expect("parse_file");
    graph.imports
}

fn find<'a>(imports: &'a [RawImport], alias: &str) -> &'a RawImport {
    imports
        .iter()
        .find(|i| i.alias.as_deref() == Some(alias))
        .unwrap_or_else(|| panic!("missing alias `{alias}` in {imports:#?}"))
}

/// The `Typedef` nodes of `src` as `(alias, aliased type)`.
/// CONTRACTS 8 / A15e: a typedef is no `RawImport`; the node carries the
/// aliased type in `type_annotation`.
fn typedefs(src: &str) -> Vec<(String, Option<String>)> {
    let graph = CProvider::new()
        .expect("CProvider init")
        .parse_file(Path::new("t.c"), src.as_bytes())
        .expect("parse_file");
    assert!(
        graph.imports.is_empty(),
        "a typedef emits no import: {:#?}",
        graph.imports
    );
    graph
        .nodes
        .into_iter()
        .filter(|n| n.kind == NodeKind::Typedef)
        .map(|n| (n.name, n.type_annotation))
        .collect()
}

fn typedef_of(src: &str, alias: &str) -> Option<String> {
    typedefs(src)
        .into_iter()
        .find(|(name, _)| name == alias)
        .unwrap_or_else(|| panic!("missing Typedef node `{alias}`"))
        .1
}

#[test]
fn test_c_typedef_primitive_emits_alias() {
    assert_eq!(
        typedef_of("typedef int Counter;\n", "Counter").as_deref(),
        Some("int")
    );
}

#[test]
fn test_c_typedef_struct_emits_alias() {
    assert_eq!(
        typedef_of("typedef struct foo { int x; } Foo;\n", "Foo").as_deref(),
        Some("struct foo")
    );
}

#[test]
fn test_c_typedef_anonymous_struct_emits_alias() {
    assert_eq!(
        typedef_of("typedef struct { int x; } Point;\n", "Point").as_deref(),
        Some("struct")
    );
}

#[test]
fn test_c_typedef_function_pointer_emits_alias() {
    // The aliased type is the return type: `*Callback` lives inside the
    // declarator that holds the alias name.
    assert_eq!(
        typedef_of("typedef int (*Callback)(int);\n", "Callback").as_deref(),
        Some("int")
    );
}

#[test]
fn test_c_typedef_pointer_alias() {
    assert_eq!(
        typedef_of("typedef char** StrArray;\n", "StrArray").as_deref(),
        Some("char **")
    );
}

#[test]
fn test_c_define_emits_alias() {
    let imports = parse("#define MAX 100\n");
    let i = find(&imports, "MAX");
    assert_eq!(i.imported_name, "MAX");
    assert_eq!(i.source, "100");
}

#[test]
fn test_c_function_macro_emits_alias() {
    let imports = parse("#define ADD(a,b) ((a)+(b))\n");
    let i = find(&imports, "ADD");
    assert_eq!(i.imported_name, "ADD");
    // Function-like macro source carries both the param list and the body
    // so the two macro flavours stay distinguishable downstream.
    assert!(
        i.source.contains("(a,b)"),
        "expected params in source, got {:?}",
        i.source
    );
    assert!(
        i.source.contains("((a)+(b))"),
        "expected body in source, got {:?}",
        i.source
    );
}

#[test]
fn test_c_extern_variable_emits_alias() {
    let imports = parse("extern int g_counter;\n");
    let i = find(&imports, "g_counter");
    assert_eq!(i.imported_name, "g_counter");
    assert_eq!(i.source, "external");
}

#[test]
fn test_c_extern_function_emits_alias() {
    let imports = parse("extern void compute(int x);\n");
    let i = find(&imports, "compute");
    assert_eq!(i.imported_name, "compute");
    assert_eq!(i.source, "external");
}

#[test]
fn test_c_include_guard_filtered() {
    // The classic `#ifndef FOO_H / #define FOO_H / #endif` pattern should
    // NOT emit a named binding — guard macros aren't real symbols.
    let imports = parse("#ifndef FOO_H\n#define FOO_H\n#endif\n");
    assert!(
        imports.iter().all(|i| i.alias.as_deref() != Some("FOO_H")),
        "include guard FOO_H should be filtered, got {imports:#?}"
    );
}

#[test]
fn test_c_include_guard_with_body_1_filtered() {
    let imports = parse("#define MY_HEADER_H 1\n");
    assert!(
        imports
            .iter()
            .all(|i| i.alias.as_deref() != Some("MY_HEADER_H")),
        "MY_HEADER_H with body `1` should be filtered, got {imports:#?}"
    );
}

#[test]
fn test_c_real_define_kept_even_with_h_lookalike() {
    // `MAX_SIZE` doesn't end with `_H` so it must pass through.
    let imports = parse("#define MAX_SIZE 4096\n");
    let i = find(&imports, "MAX_SIZE");
    assert_eq!(i.source, "4096");
}

#[test]
fn test_c_multiple_named_bindings_coexist() {
    let src = r#"
typedef int Counter;
#define MAX 100
extern int g_counter;
"#;
    let imports = parse(src);
    assert!(imports
        .iter()
        .all(|i| i.alias.as_deref() != Some("Counter")));
    find(&imports, "MAX");
    find(&imports, "g_counter");
}
