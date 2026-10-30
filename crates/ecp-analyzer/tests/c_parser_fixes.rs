//! `CProvider` defects found in the driver-move review (FU-2026-10-30-
//! 8f38f58dd41a, plus the multi-declarator type sibling of
//! FU-2026-10-30-955135c9e46b). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span = (u32, u32, u32, u32);

/// `(name, kind, span, is_exported, type_annotation)`
type Row<'g> = (&'g str, NodeKind, Span, bool, Option<&'g str>);

/// `(source, imported_name, alias, binding_kind)`
type ImportRow<'g> = (&'g str, &'g str, Option<&'g str>, Option<BindingKind>);

fn parse(path: &str, src: &[u8]) -> LocalGraph {
    CProvider::new()
        .expect("CProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn rows_of<'g>(g: &'g LocalGraph, kinds: &[NodeKind]) -> Vec<Row<'g>> {
    g.nodes
        .iter()
        .filter(|n| kinds.contains(&n.kind))
        .map(|n| {
            (
                n.name.as_str(),
                n.kind,
                n.span,
                n.is_exported,
                n.type_annotation.as_deref(),
            )
        })
        .collect()
}

fn import_rows(g: &LocalGraph) -> Vec<ImportRow<'_>> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
                i.binding_kind,
            )
        })
        .collect()
}

fn binding(name: &'static str, source: &'static str, kind: BindingKind) -> ImportRow<'static> {
    (source, name, Some(name), Some(kind))
}

const DECL_KINDS: &[NodeKind] = &[NodeKind::Function, NodeKind::Variable];

/// A header-style guard around prototypes and an `extern` variable: each
/// declaration is a node exactly as it is at file scope (prototype: no
/// `@type`; variable: the storage class in the type slice).
#[test]
fn test_c_extern_decls_inside_ifndef_guard_emit_function_and_variable() {
    let g = parse(
        "api.c",
        b"#ifndef API_H\n#define API_H\nextern int api_open(const char *path);\nextern int api_errno;\nint api_close(int fd);\n#endif\n",
    );
    assert_eq!(
        rows_of(&g, DECL_KINDS),
        [
            ("api_open", NodeKind::Function, (2, 0, 2, 38), true, None),
            (
                "api_errno",
                NodeKind::Variable,
                (3, 0, 3, 21),
                true,
                Some("extern int")
            ),
            ("api_close", NodeKind::Function, (4, 0, 4, 22), true, None),
        ]
    );
    let open = g
        .nodes
        .iter()
        .find(|n| n.name == "api_open")
        .expect("api_open");
    assert_eq!(
        open.content_hash,
        xxh3_64_bytes(b"extern int api_open(const char *path);")
    );
}

/// Every conditional branch (`#if` / `#elif` / `#else`) and a guard nested
/// two deep hold file-scope declarations; a prototype or an `extern` inside
/// a function body stays a local re-declaration with no node.
#[test]
fn test_c_decls_in_every_preproc_branch_emit_but_body_local_extern_does_not() {
    let g = parse(
        "branches.c",
        b"#if defined(USE_A)\nextern int pick_a(void);\n#elif defined(USE_B)\nextern int pick_b(void);\n#else\nint pick_c(void);\nstatic int pick_level = 1;\n#endif\nvoid f(void) {\n    int local_proto(int);\n#ifdef LOCAL\n    extern int local_counter;\n#endif\n}\n#ifndef NEST_H\n#define NEST_H\n#ifdef FEATURE\nextern int nested_fn(void);\n#endif\n#endif\n",
    );
    assert_eq!(
        rows_of(&g, DECL_KINDS),
        [
            ("pick_a", NodeKind::Function, (1, 0, 1, 24), true, None),
            ("pick_b", NodeKind::Function, (3, 0, 3, 24), true, None),
            ("pick_c", NodeKind::Function, (5, 0, 5, 17), true, None),
            (
                "pick_level",
                NodeKind::Variable,
                (6, 0, 6, 26),
                true,
                Some("static int")
            ),
            ("f", NodeKind::Function, (8, 0, 13, 1), true, Some("void")),
            ("nested_fn", NodeKind::Function, (17, 0, 17, 27), true, None),
        ]
    );
}

/// Guard: the file-scope prototype and variable keep today's records.
#[test]
fn test_c_file_scope_extern_decls_unchanged() {
    let g = parse(
        "top.c",
        b"extern int api_open(const char *path);\nextern int api_errno;\n",
    );
    assert_eq!(
        rows_of(&g, DECL_KINDS),
        [
            ("api_open", NodeKind::Function, (0, 0, 0, 38), true, None),
            (
                "api_errno",
                NodeKind::Variable,
                (1, 0, 1, 21),
                true,
                Some("extern int")
            ),
        ]
    );
}

/// A typedef whose alias sits inside a pointer, function-pointer or array
/// declarator is a `Typedef` node like the plain one beside it.
#[test]
fn test_c_pointer_fn_ptr_and_array_typedefs_emit_typedef_nodes() {
    let g = parse(
        "typedefs.c",
        b"typedef char **str_arr;\ntypedef int (*cmp_fn)(int, int);\ntypedef int int_arr[10];\ntypedef unsigned long counter_t;\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef]),
        [
            ("str_arr", NodeKind::Typedef, (0, 0, 0, 23), true, None),
            ("cmp_fn", NodeKind::Typedef, (1, 0, 1, 32), true, None),
            ("int_arr", NodeKind::Typedef, (2, 0, 2, 24), true, None),
            ("counter_t", NodeKind::Typedef, (3, 0, 3, 32), true, None),
        ]
    );
    let cmp = g.nodes.iter().find(|n| n.name == "cmp_fn").expect("cmp_fn");
    assert_eq!(
        cmp.content_hash,
        xxh3_64_bytes(b"typedef int (*cmp_fn)(int, int);")
    );
    assert!(
        rows_of(&g, &[NodeKind::Function, NodeKind::Variable]).is_empty(),
        "a typedef declarator is never a function or a variable"
    );
}

/// A data-pointer typedef import keeps its stars; a function-pointer or
/// array typedef keeps the return or element type alone, as before.
#[test]
fn test_c_pointer_typedef_import_source_keeps_stars() {
    let g = parse(
        "ptr.c",
        b"typedef char **str_arr;\ntypedef struct node *node_p;\ntypedef int (*cmp_fn)(int);\ntypedef int int_arr[4];\n",
    );
    assert_eq!(
        import_rows(&g),
        [
            binding("str_arr", "char **", BindingKind::Alias),
            binding("node_p", "struct node *", BindingKind::Alias),
            binding("cmp_fn", "int", BindingKind::Alias),
            binding("int_arr", "int", BindingKind::Alias),
        ]
    );
}

/// Named-binding imports follow source order, a nested preproc block
/// included.
#[test]
fn test_c_named_binding_imports_keep_source_order() {
    let g = parse(
        "order.c",
        b"#define FIRST 1\ntypedef int first_t;\nextern int first_g;\n#ifdef FEATURE\n#define INNER 2\n#endif\n#define LAST 3\n",
    );
    assert_eq!(
        import_rows(&g),
        [
            binding("FIRST", "1", BindingKind::Constant),
            binding("first_t", "int", BindingKind::Alias),
            binding("first_g", "external", BindingKind::Alias),
            binding("INNER", "2", BindingKind::Constant),
            binding("LAST", "3", BindingKind::Constant),
        ]
    );
}

/// `#ifndef X` / `#define X` (empty body) is an include guard: no Macro
/// node, and the raw-source fallback must not re-add one. The real macro
/// inside stays.
#[test]
fn test_c_include_guard_define_emits_no_macro_node() {
    let g = parse(
        "api.c",
        b"#ifndef API_H\n#define API_H\n#define API_VERSION 2\n#endif\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Macro]),
        [("API_VERSION", NodeKind::Macro, (2, 0, 3, 0), true, None)]
    );
    assert!(g.nodes.iter().all(|n| n.name != "API_H"), "{:?}", g.nodes);
    assert_eq!(
        import_rows(&g),
        [binding("API_VERSION", "2", BindingKind::Constant)]
    );
}

/// Atypical guard spellings: a Google-style `_H_` name and the
/// `#if !defined(X)` form. `#ifndef BUF_SIZE` / `#define BUF_SIZE 4096` has
/// a body, so it is a default value, not a guard: its Macro and import stay.
/// Imports keep their own name-suffix rule, so `WIDGET_H_` stays a Flag.
#[test]
fn test_c_include_guard_spellings_skipped_but_default_value_define_kept() {
    let g = parse(
        "widget.c",
        b"#ifndef WIDGET_H_\n#define WIDGET_H_\n#define WIDGET_MAX 8\n#ifndef BUF_SIZE\n#define BUF_SIZE 4096\n#endif\n#endif\n#if !defined(LEGACY_H)\n#define LEGACY_H\n#define LEGACY_ON 1\n#endif\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Macro]),
        [
            ("WIDGET_MAX", NodeKind::Macro, (2, 0, 3, 0), true, None),
            ("BUF_SIZE", NodeKind::Macro, (4, 0, 5, 0), true, None),
            ("LEGACY_ON", NodeKind::Macro, (9, 0, 10, 0), true, None),
        ]
    );
    let mut imports = import_rows(&g);
    imports.sort_by_key(|i| i.1);
    assert_eq!(
        imports,
        [
            binding("BUF_SIZE", "4096", BindingKind::Constant),
            binding("LEGACY_ON", "1", BindingKind::Constant),
            binding("WIDGET_H_", "", BindingKind::Flag),
            binding("WIDGET_MAX", "8", BindingKind::Constant),
        ]
    );
}

/// `int x, y;` declares two `int`s: each declarator's type is the shared
/// specifier plus its own pointer prefix, never the text of the declarator
/// before it. Span and hash stay the whole declaration's, as in Java and C#.
#[test]
fn test_c_multi_declarator_types_exclude_previous_declarator() {
    let g = parse(
        "multi.c",
        b"struct P { int x, y; char *a, *b; int *p, q; };\nint gx, gy;\n",
    );
    let s = Some;
    assert_eq!(
        rows_of(&g, &[NodeKind::Property, NodeKind::Variable]),
        [
            ("x", NodeKind::Property, (0, 11, 0, 20), true, s("int")),
            ("y", NodeKind::Property, (0, 11, 0, 20), true, s("int")),
            ("a", NodeKind::Property, (0, 21, 0, 33), true, s("char *")),
            ("b", NodeKind::Property, (0, 21, 0, 33), true, s("char *")),
            ("p", NodeKind::Property, (0, 34, 0, 44), true, s("int *")),
            ("q", NodeKind::Property, (0, 34, 0, 44), true, s("int")),
            ("gx", NodeKind::Variable, (1, 0, 1, 11), true, s("int")),
            ("gy", NodeKind::Variable, (1, 0, 1, 11), true, s("int")),
        ]
    );
    let hash_of = |name: &str| {
        g.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap()
            .content_hash
    };
    assert_eq!(hash_of("x"), xxh3_64_bytes(b"int x, y;"));
    assert_eq!(hash_of("y"), xxh3_64_bytes(b"int x, y;"));
    assert_eq!(hash_of("gy"), xxh3_64_bytes(b"int gx, gy;"));
}

fn macro_names(g: &LocalGraph) -> Vec<&str> {
    let mut names: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Macro)
        .map(|n| n.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

/// A `#define` on the line after one ending in `\` is part of that macro's
/// body (the preprocessor splices the lines), so it defines nothing. The
/// macros before and after the continued pair stay. Covers LF and CRLF.
#[test]
fn test_c_define_inside_continued_macro_body_emits_no_macro_node() {
    let lf = parse(
        "cont.c",
        b"#define FIRST 1\n#define OUTER(x) \\\n#define NESTED 1\n#define AFTER 2\n",
    );
    assert_eq!(macro_names(&lf), ["AFTER", "FIRST", "OUTER"]);

    let crlf = parse(
        "cont_crlf.c",
        b"#define OUTER 1 \\\r\n#define NESTED 2\r\n#define AFTER 3\r\n",
    );
    assert_eq!(macro_names(&crlf), ["AFTER", "OUTER"]);
}

/// Guard: a `\` continuation that is followed by a plain body line does not
/// hide the next real `#define`.
#[test]
fn test_c_define_after_finished_continuation_is_kept() {
    let g = parse(
        "cont_end.c",
        b"#define LONG(x) \\\n    ((x) + 1)\n#define NEXT 2\n",
    );
    assert_eq!(macro_names(&g), ["LONG", "NEXT"]);
}
