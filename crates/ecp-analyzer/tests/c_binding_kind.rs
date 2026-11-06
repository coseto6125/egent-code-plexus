//! C `BindingKind` classification: each `#define` and `typedef` emitted by
//! the C parser carries a `binding_kind: Some(_)` that reflects the body
//! shape. This file verifies the four categories individually and together.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, RawImport};
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

/// Stands for: a typedef a C file declares is a local binding, never a
/// module import of that file (it was an alias `RawImport`, which the graph
/// turned into `Imports main.c -> main.c::cb_t`). Every declarator shape
/// holds, and the file's `#include` still yields its own import.
#[test]
fn test_extract_named_bindings_c_local_typedef_emits_no_import() {
    let src = "#include \"t.h\"\n\
        typedef int Counter;\n\
        typedef void (*cb_t)(int *);\n\
        typedef char **StrArray;\n\
        typedef int IntArr[10];\n\
        typedef struct { int a; } Al;\n\
        typedef int A, *B;\n";
    let imports = parse(src);
    for alias in ["Counter", "cb_t", "StrArray", "IntArr", "Al", "A", "B"] {
        assert!(
            imports.iter().all(|i| i.alias.as_deref() != Some(alias)),
            "typedef `{alias}` must emit no import: {imports:#?}"
        );
    }
    assert_eq!(
        imports.iter().filter(|i| i.alias.is_none()).count(),
        1,
        "the `#include` keeps its file import: {imports:#?}"
    );
}

/// Stands for: the control of the typedef case: `#define` and `extern`
/// declarations keep their alias imports, so only the typedef left.
#[test]
fn test_extract_named_bindings_c_define_and_extern_still_emit_alias_imports() {
    let imports = parse("typedef int Counter;\n#define alloc malloc\nextern int g_counter;\n");
    assert_eq!(
        find(&imports, "alloc").binding_kind,
        Some(BindingKind::Alias)
    );
    assert_eq!(
        find(&imports, "g_counter").binding_kind,
        Some(BindingKind::Alias)
    );
}

#[test]
fn test_define_numeric_is_constant() {
    let imports = parse("#define MAX 100\n");
    let i = find(&imports, "MAX");
    assert_eq!(i.binding_kind, Some(BindingKind::Constant));
}

#[test]
fn test_define_string_literal_is_constant() {
    let imports = parse("#define VERSION \"v1.2\"\n");
    let i = find(&imports, "VERSION");
    assert_eq!(i.binding_kind, Some(BindingKind::Constant));
}

#[test]
fn test_define_empty_body_is_flag() {
    // Non-guard empty body — DEBUG does not match guard suffix patterns.
    let imports = parse("#define DEBUG\n");
    let i = find(&imports, "DEBUG");
    assert_eq!(i.binding_kind, Some(BindingKind::Flag));
}

#[test]
fn test_define_identifier_body_is_alias() {
    // Identifier-bodied #define: symbol → symbol portability shim.
    let imports = parse("#define alloc malloc\n");
    let i = find(&imports, "alloc");
    assert_eq!(i.binding_kind, Some(BindingKind::Alias));
}

#[test]
fn test_function_like_macro_is_macro() {
    let imports = parse("#define ADD(a,b) ((a)+(b))\n");
    let i = find(&imports, "ADD");
    assert_eq!(i.binding_kind, Some(BindingKind::Macro));
}

#[test]
fn test_define_expression_body_is_macro() {
    // Parenthesized bit-shift expression.
    let imports = parse("#define BUFSIZE (1<<12)\n");
    let i = find(&imports, "BUFSIZE");
    assert_eq!(i.binding_kind, Some(BindingKind::Macro));
}

#[test]
fn test_extern_decl_is_alias() {
    let imports = parse("extern int g_counter;\n");
    let i = find(&imports, "g_counter");
    assert_eq!(i.binding_kind, Some(BindingKind::Alias));
}

#[test]
fn test_normal_include_has_no_binding_kind() {
    let imports = parse("#include <stdio.h>\n");
    // The include emits an RawImport with alias=None and binding_kind=None.
    let include = imports.iter().find(|i| i.alias.is_none());
    assert!(include.is_some(), "expected a #include import");
    assert_eq!(include.unwrap().binding_kind, None);
}
