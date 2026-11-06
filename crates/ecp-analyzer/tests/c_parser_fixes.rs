//! `CProvider` defects found in the driver-move review (FU-2026-10-30-
//! 8f38f58dd41a, plus the multi-declarator type sibling of
//! FU-2026-10-30-955135c9e46b). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph};
use ecp_core::graph::{CallMeta, NodeKind};
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

/// `(alias, aliased type)` of every `Typedef` node, in source order
/// (the aliased type lives on the node, not an import).
fn typedef_types(g: &LocalGraph) -> Vec<(&str, Option<&str>)> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Typedef)
        .map(|n| (n.name.as_str(), n.type_annotation.as_deref()))
        .collect()
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
                false,
                Some("static int")
            ),
            ("f", NodeKind::Function, (8, 0, 13, 1), true, Some("void")),
            ("nested_fn", NodeKind::Function, (17, 0, 17, 27), true, None),
        ]
    );
}

/// Guard: a file-scope prototype is an exported Function with no type
/// annotation, and a file-scope `extern` variable is a Variable whose
/// annotation is `extern int`.
#[test]
fn test_c_file_scope_extern_decls_emit_function_and_extern_variable() {
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
            (
                "str_arr",
                NodeKind::Typedef,
                (0, 0, 0, 23),
                true,
                Some("char **")
            ),
            (
                "cmp_fn",
                NodeKind::Typedef,
                (1, 0, 1, 32),
                true,
                Some("int")
            ),
            (
                "int_arr",
                NodeKind::Typedef,
                (2, 0, 2, 24),
                true,
                Some("int")
            ),
            (
                "counter_t",
                NodeKind::Typedef,
                (3, 0, 3, 32),
                true,
                Some("unsigned long")
            ),
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

/// A data-pointer typedef keeps its stars in the `Typedef` node's
/// `type_annotation`; a function-pointer or array typedef keeps the return
/// or element type alone. A typedef is no import.
#[test]
fn test_c_pointer_typedef_import_source_keeps_stars() {
    let g = parse(
        "ptr.c",
        b"typedef char **str_arr;\ntypedef struct node *node_p;\ntypedef int (*cmp_fn)(int);\ntypedef int int_arr[4];\n",
    );
    assert_eq!(import_rows(&g), []);
    assert_eq!(
        typedef_types(&g),
        [
            ("str_arr", Some("char **")),
            ("node_p", Some("struct node *")),
            ("cmp_fn", Some("int")),
            ("int_arr", Some("int")),
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
/// A guard is neither a Macro node nor an import, whatever rule finds it.
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

// ---- Round 2 (P5 group A) ----

fn macro_rows(g: &LocalGraph) -> Vec<Row<'_>> {
    let mut rows = rows_of(g, &[NodeKind::Macro]);
    rows.sort_by_key(|r| (r.2, r.0));
    rows
}

fn sorted_rows_of<'g>(g: &'g LocalGraph, kinds: &[NodeKind]) -> Vec<Row<'g>> {
    let mut rows = rows_of(g, kinds);
    rows.sort_by_key(|r| (r.2, r.0));
    rows
}

/// A1: a function-pointer typedef whose return type is a pointer nests the
/// alias one `pointer_declarator` deeper (`lua.h`, `redismodule.h`,
/// `defrag.c`); it is a `Typedef` node like `typedef int (*cmp_fn)(int);`.
#[test]
fn test_c_pointer_return_fn_ptr_typedefs_emit_typedef_nodes() {
    let g = parse(
        "lua.c",
        b"typedef const char * (*lua_Reader) (lua_State *L, void *ud, size_t *sz);\ntypedef void *(*RedisModuleTypeLoadFunc)(RedisModuleIO *rdb, int encver);\ntypedef dict *(*getClientChannelsFn)(client *);\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef]),
        [
            (
                "lua_Reader",
                NodeKind::Typedef,
                (0, 0, 0, 72),
                true,
                Some("const char *")
            ),
            (
                "RedisModuleTypeLoadFunc",
                NodeKind::Typedef,
                (1, 0, 1, 73),
                true,
                Some("void *")
            ),
            (
                "getClientChannelsFn",
                NodeKind::Typedef,
                (2, 0, 2, 47),
                true,
                Some("dict *")
            ),
        ]
    );
    let reader = g
        .nodes
        .iter()
        .find(|n| n.name == "lua_Reader")
        .expect("lua_Reader");
    assert_eq!(
        reader.content_hash,
        xxh3_64_bytes(b"typedef const char * (*lua_Reader) (lua_State *L, void *ud, size_t *sz);")
    );
    assert!(
        rows_of(&g, &[NodeKind::Function, NodeKind::Variable]).is_empty(),
        "a typedef declarator is never a function or a variable"
    );
}

/// A1 atypical: `***`, an array of function pointers, a pointer to a
/// function pointer, a `**` return, and the A1 shape inside `#ifdef` are all
/// `Typedef` nodes. The abstract `typedef void *(*)(int);` names nothing, so
/// it emits no node. `cmp_fn`, `counter_t` and the pointer-returning
/// prototype `alloc` stay a Typedef, a Typedef and a Function.
#[test]
fn test_c_nested_fn_ptr_and_triple_pointer_typedefs_emit_typedef_nodes() {
    let g = parse(
        "typedefs2.c",
        b"typedef char ***str_cube;\ntypedef int (*handlers[4])(int);\ntypedef int (**handler_pp)(int);\ntypedef char **(*argv_fn)(void);\n#ifdef HAVE_ALLOC\ntypedef void *(*alloc_fn)(size_t);\n#endif\ntypedef void *(*)(int);\ntypedef int (*cmp_fn)(int, int);\ntypedef unsigned long counter_t;\nvoid *alloc(size_t n);\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef, NodeKind::Function]),
        [
            (
                "str_cube",
                NodeKind::Typedef,
                (0, 0, 0, 25),
                true,
                Some("char ***")
            ),
            (
                "handlers",
                NodeKind::Typedef,
                (1, 0, 1, 32),
                true,
                Some("int")
            ),
            (
                "handler_pp",
                NodeKind::Typedef,
                (2, 0, 2, 32),
                true,
                Some("int")
            ),
            (
                "argv_fn",
                NodeKind::Typedef,
                (3, 0, 3, 32),
                true,
                Some("char **")
            ),
            (
                "alloc_fn",
                NodeKind::Typedef,
                (5, 0, 5, 34),
                true,
                Some("void *")
            ),
            (
                "cmp_fn",
                NodeKind::Typedef,
                (8, 0, 8, 32),
                true,
                Some("int")
            ),
            (
                "counter_t",
                NodeKind::Typedef,
                (9, 0, 9, 32),
                true,
                Some("unsigned long")
            ),
            ("alloc", NodeKind::Function, (10, 0, 10, 22), true, None),
        ]
    );
    assert!(
        g.nodes.iter().all(|n| !n.name.is_empty()),
        "the abstract declarator names nothing: {:?}",
        g.nodes
    );
}

/// A2: the file-level guard decision reads the source, not the recovered
/// tree. Here the `#if` / `#else` branches each open a brace, so the whole
/// guard lands in an ERROR node (`hiredis/adapters/libuv.h` shape). The
/// guard `#define` still emits no Macro; the real macro stays.
#[test]
fn test_c_include_guard_in_error_recovered_tree_emits_no_macro_node() {
    let g = parse(
        "libuv.c",
        b"#ifndef __HIREDIS_LIBUV_H__\n#define __HIREDIS_LIBUV_H__\n#define LIBUV_KEEP 1\nint f() {\n#if A\n  if (a) {\n#else\n  if (b) {\n#endif\n  }\n}\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["LIBUV_KEEP"]);
}

/// A2: a guard nested inside the file-level guard is no file-level guard,
/// so its `#define` stays a Macro even when error recovery closes the outer
/// guard early and lifts the nested one to the top of the tree
/// (`json_v3_10_5.hpp` `INCLUDE_NLOHMANN_JSON_FWD_HPP_` shape).
#[test]
fn test_c_nested_guard_lifted_by_error_recovery_keeps_macro_node() {
    let g = parse(
        "json.c",
        b"#ifndef INCLUDE_NLOHMANN_JSON_HPP_\n#define INCLUDE_NLOHMANN_JSON_HPP_\nint pick() {\n#if A\n  return 1; }\n#else\n  return 2; }\n#endif\n#ifndef INCLUDE_NLOHMANN_JSON_FWD_HPP_\n#define INCLUDE_NLOHMANN_JSON_FWD_HPP_\nint fwd;\n#endif\n#endif\n",
    );
    assert_eq!(
        macro_rows(&g),
        [(
            "INCLUDE_NLOHMANN_JSON_FWD_HPP_",
            NodeKind::Macro,
            (9, 0, 10, 0),
            true,
            None
        )]
    );
}

/// A2 guard: on a clean tree, two nested guards side by side keep their
/// Macros, two file-level guards side by side lose theirs, and a `#define`
/// whose name differs from the `#ifndef` is no guard.
#[test]
fn test_c_guard_neighbours_on_clean_tree_unchanged() {
    let g = parse(
        "guards.c",
        b"#ifndef OUTER_H\n#define OUTER_H\n#ifndef INNER_H\n#define INNER_H\n#endif\n#ifndef PEER_H\n#define PEER_H\n#endif\n#endif\n#ifndef SECOND_H\n#define SECOND_H\n#endif\n#ifndef WRONG_H\n#define OTHER_H\n#endif\n",
    );
    assert_eq!(
        macro_rows(&g),
        [
            ("INNER_H", NodeKind::Macro, (3, 0, 4, 0), true, None),
            ("PEER_H", NodeKind::Macro, (6, 0, 7, 0), true, None),
            ("OTHER_H", NodeKind::Macro, (13, 0, 14, 0), true, None),
        ]
    );
}

/// A2 edge: an empty file and a guard with no name emit no Macro.
#[test]
fn test_c_empty_file_and_nameless_guard_emit_no_macro_node() {
    assert!(parse("empty.c", b"").nodes.is_empty());
    let g = parse("noname.c", b"#ifndef\n#define\n#endif\n");
    assert!(macro_names(&g).is_empty(), "{:?}", g.nodes);
}

/// `(caller_name, caller_span, call_index, flags, dispatch_type)`
type CallMetaRow<'g> = (&'g str, Span, u32, u8, &'g str);

fn call_meta_rows(g: &LocalGraph) -> Vec<CallMetaRow<'_>> {
    g.call_metas
        .iter()
        .map(|m| {
            (
                m.caller_name.as_str(),
                m.caller_span,
                m.call_index,
                m.flags,
                m.dispatch_type.as_str(),
            )
        })
        .collect()
}

/// A3: a function-pointer parameter types the calls of its own function
/// only. `direct` calls the file-scope function `cb`, which `run`'s
/// parameter `cb` does not shadow, so that call is direct. The file-scope
/// `hook` still types the call in `fire`, and `run`'s call inside a nested
/// block still goes through its parameter.
#[test]
fn test_c_fn_ptr_param_types_calls_in_its_own_function_only() {
    let g = parse(
        "dispatch.c",
        b"typedef int (*handler_t)(int);\nstatic handler_t hook;\nint cb(int x) { return x; }\nint run(handler_t cb) { if (cb) { return cb(1); } return 0; }\nint direct(void) { return cb(2); }\nint fire(void) { return hook(3); }\n",
    );
    let callback = CallMeta::FLAG_CALLBACK;
    assert_eq!(
        call_meta_rows(&g),
        [
            ("run", (3, 0, 3, 61), 0, callback, "handler_t"),
            ("fire", (5, 0, 5, 34), 0, callback, "handler_t"),
        ]
    );
}

/// A4: a file-scope array declaration is a Variable like a scalar one,
/// guarded or not (`tre/tests/retest.c:64-66`, `jemalloc/src/tsd.c:235`).
/// The type keeps the declaration prefix, as for a struct field array.
#[test]
fn test_c_file_scope_array_decls_emit_variable_nodes() {
    let g = parse(
        "retest.c",
        b"#define MAXSTRSIZE 8192\nstatic wchar_t wstr[MAXSTRSIZE];\nstatic int woffs[MAXSTRSIZE];\n#if defined(JEMALLOC_MALLOC_THREAD_CLEANUP) || defined(_WIN32)\nstatic unsigned             ncleanups;\nstatic malloc_tsd_cleanup_t cleanups[MALLOC_TSD_CLEANUPS_MAX];\n#endif\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable]),
        [
            (
                "wstr",
                NodeKind::Variable,
                (1, 0, 1, 32),
                false,
                s("static wchar_t")
            ),
            (
                "woffs",
                NodeKind::Variable,
                (2, 0, 2, 29),
                false,
                s("static int")
            ),
            (
                "ncleanups",
                NodeKind::Variable,
                (4, 0, 4, 38),
                false,
                s("static unsigned")
            ),
            (
                "cleanups",
                NodeKind::Variable,
                (5, 0, 5, 62),
                false,
                s("static malloc_tsd_cleanup_t")
            ),
        ]
    );
    let wstr = g.nodes.iter().find(|n| n.name == "wstr").expect("wstr");
    assert_eq!(
        wstr.content_hash,
        xxh3_64_bytes(b"static wchar_t wstr[MAXSTRSIZE];")
    );
}

/// A4 atypical: an initialised array, an array of pointers, a 2-D array and
/// an array beside a scalar in one declaration are Variables (a `const`
/// array a Const). A `static` array inside a body is a local and a struct
/// array member stays a Property.
#[test]
fn test_c_initialised_pointer_and_2d_arrays_emit_variables_but_local_does_not() {
    let g = parse(
        "arrays.c",
        b"static const uint8_t bitsinbyte[256] = {0};\nstatic char *bio_worker_title[] = {\"a\", \"b\"};\nstatic uint64_t combine_cache[64][64];\nint pair[2], single;\nstruct S { int buf[8]; };\nvoid f(void) { static int local_arr[4]; }\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(
            &g,
            &[NodeKind::Variable, NodeKind::Const, NodeKind::Property]
        ),
        [
            (
                "bitsinbyte",
                NodeKind::Const,
                (0, 0, 0, 43),
                false,
                s("static const uint8_t")
            ),
            (
                "bio_worker_title",
                NodeKind::Variable,
                (1, 0, 1, 45),
                false,
                s("static char *")
            ),
            (
                "combine_cache",
                NodeKind::Variable,
                (2, 0, 2, 38),
                false,
                s("static uint64_t")
            ),
            ("pair", NodeKind::Variable, (3, 0, 3, 20), true, s("int")),
            ("single", NodeKind::Variable, (3, 0, 3, 20), true, s("int")),
            ("buf", NodeKind::Property, (4, 11, 4, 22), true, s("int")),
        ]
    );
}

/// A5 (`jemalloc/src/pages.c:636`): a `#ifdef` split inside a string
/// argument puts the rest of the file in one top-level ERROR node. The
/// file-scope declarations after the function, guarded or not, are still
/// nodes (a `static` prototype is an unexported Function). The locals of
/// the broken body (`before` at brace depth 1, `buf` at depth 2) are not.
#[test]
fn test_c_file_scope_decls_after_error_recovered_function_emit_variables() {
    let g = parse(
        "pages.c",
        b"static void\nunmap(void *addr) {\n\tint before = 0;\n#ifdef _WIN32\n\tif (VirtualFree(addr) == 0)\n#else\n\tif (munmap(addr) == -1)\n#endif\n\t{\n\t\tchar buf[64];\n\t\tprintf(\n\t\t    \"Error in \"\n#ifdef _WIN32\n\t\t    \"VirtualFree\"\n#else\n\t\t    \"munmap\"\n#endif\n\t\t    \"(): %s\\n\",\n\t\t    buf);\n\t}\n}\n\n#ifdef HAVE_GATE\nstatic int gate = 1;\n#endif\nstatic int plain;\nstatic int init_gate(void);\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable, NodeKind::Function]),
        [
            (
                "gate",
                NodeKind::Variable,
                (23, 0, 23, 20),
                false,
                s("static int")
            ),
            (
                "plain",
                NodeKind::Variable,
                (25, 0, 25, 17),
                false,
                s("static int")
            ),
            (
                "init_gate",
                NodeKind::Function,
                (26, 0, 26, 27),
                false,
                None
            ),
        ]
    );
}

/// A5 (`src/setproctitle.c:57`): `extern char **environ;` is a Variable in
/// nested `#if` blocks and at file scope; the cause is the `**` declarator,
/// not the conditional. The one-star neighbour keeps its record and a `**`
/// local stays out.
#[test]
fn test_c_double_pointer_file_scope_decls_emit_variables() {
    let g = parse(
        "setproctitle.c",
        b"#if !HAVE_SETPROCTITLE\n#if (defined __linux || defined __APPLE__)\nextern char **environ;\n#endif\n#endif\nstatic char **argv_copy = 0;\nextern char *single_ptr;\nvoid f(void) { char **local_pp = 0; }\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable]),
        [
            (
                "environ",
                NodeKind::Variable,
                (2, 0, 2, 22),
                true,
                s("extern char **")
            ),
            (
                "argv_copy",
                NodeKind::Variable,
                (5, 0, 5, 28),
                false,
                s("static char **")
            ),
            (
                "single_ptr",
                NodeKind::Variable,
                (6, 0, 6, 24),
                true,
                s("extern char *")
            ),
        ]
    );
}

// ---- Round 2 phase B: siblings and off-task fixes ----

/// A5 atypical: the brace depth that decides file scope under an ERROR
/// node skips braces in char and string literals, comments and directives,
/// and counts only the first branch of an `#if` / `#else` whose branches
/// each open a brace. `local` sits at depth 1 and stays out.
#[test]
fn test_c_brace_depth_skips_literals_comments_directives_and_later_branches() {
    let g = parse(
        "depth.c",
        b"int pick(int a) {\n  int local = a;\n#if FAST\n  if (a) {\n#else\n  if (!a) {\n#endif\n    a++;\n  }\n  return a;\n}\nstatic int open_brace = '{';\nstatic const char *close_text = \"}}\";\n/* a stray { in a comment */\n#define BLOCK_OPEN {\n#if defined(LEGACY)\nstatic int legacy_gate;\n#else\nstatic int modern_gate;\n#endif\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable]),
        [
            (
                "open_brace",
                NodeKind::Variable,
                (11, 0, 11, 28),
                false,
                s("static int")
            ),
            (
                "close_text",
                NodeKind::Variable,
                (12, 0, 12, 37),
                false,
                s("static const char *")
            ),
            (
                "legacy_gate",
                NodeKind::Variable,
                (16, 0, 16, 23),
                false,
                s("static int")
            ),
            (
                "modern_gate",
                NodeKind::Variable,
                (18, 0, 18, 23),
                false,
                s("static int")
            ),
        ]
    );
}

/// A2 atypical: comments before the guard, after its condition and between
/// the condition and the `#define` leave it a guard; a `#if` inside a block
/// comment is no conditional. The real macro stays.
#[test]
fn test_c_include_guard_around_comments_emits_no_macro_node() {
    let g = parse(
        "cfg.c",
        b"/* header\n#if FAKE\n*/\n#ifndef CFG_H // guard\n/* note */\n#define CFG_H 1\n#define CFG_ON 1\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["CFG_ON"]);
}

/// A1 sibling: a function-type typedef and an array-of-pointers typedef
/// are `Typedef` nodes too.
#[test]
fn test_c_function_type_and_pointer_array_typedefs_emit_typedef_nodes() {
    let g = parse(
        "fntype.c",
        b"typedef void handler_fn(int);\ntypedef char *names_t[4];\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef]),
        [
            (
                "handler_fn",
                NodeKind::Typedef,
                (0, 0, 0, 29),
                true,
                Some("void")
            ),
            (
                "names_t",
                NodeKind::Typedef,
                (1, 0, 1, 25),
                true,
                Some("char *")
            ),
        ]
    );
}

/// Off-task 2: the leading stars of a typedef declarator belong to the
/// returned or element type, so the `Typedef` node's `type_annotation` keeps
/// them (`void *`, `char **`, `const char *`, `char *`). A function-pointer
/// typedef with a plain return type and a data-pointer typedef keep today's
/// text. The type moved from the import to the node.
#[test]
fn test_c_pointer_return_typedef_import_source_keeps_return_stars() {
    let g = parse(
        "returns.c",
        b"typedef void *(*alloc_fn)(size_t);\ntypedef char **(*argv_fn)(void);\ntypedef const char * (*lua_Reader) (lua_State *L);\ntypedef char *names_t[4];\ntypedef int (*cmp_fn)(int);\ntypedef char **str_arr;\n",
    );
    assert_eq!(import_rows(&g), []);
    assert_eq!(
        typedef_types(&g),
        [
            ("alloc_fn", Some("void *")),
            ("argv_fn", Some("char **")),
            ("lua_Reader", Some("const char *")),
            ("names_t", Some("char *")),
            ("cmp_fn", Some("int")),
            ("str_arr", Some("char **")),
        ]
    );
}

/// A function-pointer member whose function returns a pointer, one or two
/// levels deep, is a Property with no type slice, and its parameter names
/// stay no member.
#[test]
fn test_c_function_pointer_member_pointer_return_emits_property() {
    let g = parse(
        "alloc.h",
        b"struct alloc_ops {\n    void *(*alloc)(int size);\n    char **(*argv)(void);\n    int (*plain)(int n);\n};\n",
    );
    let names: Vec<(&str, Option<&str>)> = rows_of(&g, &[NodeKind::Property])
        .into_iter()
        .map(|(name, _, _, _, ty)| (name, ty))
        .collect();
    assert_eq!(names, [("alloc", None), ("argv", None), ("plain", None)]);
}

/// glibc and the kernel uapi headers write `__extension__ typedef ..`: the
/// GNU keyword before `typedef` keeps the aliased type on the node.
#[test]
fn test_c_extension_keyword_typedef_keeps_aliased_type() {
    let g = parse(
        "types.h",
        b"__extension__ typedef unsigned long long u64;\n__extension__ typedef struct { int a; } pair_t;\n",
    );
    assert_eq!(import_rows(&g), []);
    assert_eq!(typedef_types(&g)[0], ("u64", Some("unsigned long long")));
}

/// A3 sibling: a prototype's parameter is visible in the prototype only,
/// so `install`'s `handler_t cb` never types the call of the function `cb`.
#[test]
fn test_c_prototype_param_does_not_type_calls_elsewhere() {
    let g = parse(
        "proto.c",
        b"typedef int (*handler_t)(int);\nint install(handler_t cb);\nint cb(int x) { return x; }\nint direct(void) { return cb(2); }\n",
    );
    assert!(g.call_metas.is_empty(), "{:?}", call_meta_rows(&g));
}

// ---- Round 3 (P6 group A) ----

/// A-R1 (`jemalloc/test/unit/bit_util.c:245-246`): a comment in a
/// multi-line `#define` ends its `preproc_arg`, so the rest of the macro
/// body parses as code. Its locals are no Variables; the global before the
/// macro stays.
#[test]
fn test_c_macro_body_locals_after_comment_emit_no_variable() {
    let g = parse(
        "bit_util.c",
        b"static int kept = 1;\n#define TEST_POPCOUNT(t, suf, pri_hex) do {\t\t\t\t\\\n\tt bmul = (t)0x0101010101010101ULL;\t\t\t\t\\\n\tfor (unsigned i = 0; i < (1 << sizeof(t)); i++) {\t\t\\\n\t\tfor (unsigned j = 0; j < 256; j++) {\t\t\t\\\n\t\t\t/*\t\t\t\t\t\t\\\n\t\t\t * Replicate the byte j into various\t\t\\\n\t\t\t * bytes of the integer (as indicated by the\t\\\n\t\t\t * mask in i), and ensure that the popcount of\t\\\n\t\t\t * the result is popcount(i) * popcount(j)\t\\\n\t\t\t */\t\t\t\t\t\t\\\n\t\t\tt mask = (t)expand_byte_to_mask(i);\t\t\\\n\t\t\tt x = (bmul * j) & mask;\t\t\t\\\n\t\t\texpect_u_eq(\t\t\t\t\t\\\n\t\t\t    popcount_byte(i) * popcount_byte(j),\t\\\n\t\t\t    popcount_##suf(x),\t\t\t\t\\\n\t\t\t    \"Unexpected result, x=0x%\"pri_hex, x);\t\\\n\t\t}\t\t\t\t\t\t\t\\\n\t}\t\t\t\t\t\t\t\t\\\n} while (0)\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Variable]),
        [(
            "kept",
            NodeKind::Variable,
            (0, 0, 0, 20),
            false,
            Some("static int")
        )]
    );
}

/// A-R3, C side (`deps/xxhash/xxhash.h:2655`): both grammars read
/// `typedef __attribute__((aligned(1))) T x;` as a typedef cut short at the
/// attribute, with a missing `;`, and `T x;` as a declaration on the same
/// row. Neither a Typedef nor a named binding takes the name `__aligned__`:
/// the alias is `x`, a Typedef spanning both statements and an `Alias`
/// binding of `T`, and `x` is no Variable. A macro-call type stays a
/// Typedef with its alias.
#[test]
fn test_c_attribute_typedef_takes_alias_of_tail_declaration() {
    let g = parse(
        "witness.c",
        b"typedef ql_head(witness_t) witness_list_t;\ntypedef __attribute__((__aligned__(1))) unsigned int xxh_unalign32;\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef, NodeKind::Variable]),
        [
            (
                "witness_list_t",
                NodeKind::Typedef,
                (0, 0, 0, 42),
                true,
                Some("ql_head(witness_t)")
            ),
            (
                "xxh_unalign32",
                NodeKind::Typedef,
                (1, 0, 1, 67),
                true,
                Some("unsigned int")
            ),
        ]
    );
    assert_eq!(
        named_content_hash(&g, "xxh_unalign32"),
        xxh3_64_bytes(b"typedef __attribute__((__aligned__(1))) unsigned int xxh_unalign32;")
    );
    assert_eq!(import_rows(&g), []);
}

fn named_content_hash(g: &LocalGraph, name: &str) -> u64 {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node `{name}` in {:?}", g.nodes))
        .content_hash
}

/// A-R5 (`src/adlist.c:46`, `deps/jemalloc/src/extent.c:1398`): the
/// declarator decides pointer-ness, never the spacing around `*`. The
/// parameter `list *list` types the call through its function-pointer
/// field, and a function-pointer parameter types the call through it. A
/// prototype declares a function, not a variable: the direct call of a
/// prototyped function carries no call meta.
#[test]
fn test_c_pointer_declarator_types_calls_and_prototype_types_none() {
    let g = parse(
        "adlist.c",
        b"typedef struct list { void (*free)(void *ptr); } list;\nvoid listEmpty(list *list) { list->free(0); }\nstatic edata_t *extent_split(int n);\nint caller(void) { return extent_split(1) != 0; }\nint run(int (*cb)(int)) { return cb(1); }\n",
    );
    assert_eq!(
        call_meta_rows(&g),
        [
            (
                "listEmpty",
                (1, 0, 1, 45),
                0,
                CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH,
                "list"
            ),
            ("run", (4, 0, 4, 41), 0, CallMeta::FLAG_CALLBACK, "int"),
        ]
    );
}

/// A-R7, C side: an anonymous struct type is named by its keyword and a
/// named one by keyword and tag, for a variable and for a struct field;
/// the body is no part of the type text.
#[test]
fn test_c_struct_body_is_no_part_of_type_annotation() {
    let g = parse(
        "anon.c",
        b"static struct { int a; } anon;\nstruct named_s { int b; } named;\nstruct outer { struct { int x; } inner; int y; };\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Variable]),
        [
            (
                "anon",
                NodeKind::Variable,
                (0, 0, 0, 30),
                false,
                Some("static struct")
            ),
            (
                "named",
                NodeKind::Variable,
                (1, 0, 1, 32),
                true,
                Some("struct named_s")
            ),
        ]
    );
    let inner = g
        .nodes
        .iter()
        .find(|n| n.name == "inner" && n.kind == NodeKind::Property)
        .expect("Property `inner`");
    assert_eq!(inner.type_annotation.as_deref(), Some("struct"));
}

// Round 3 review, redis `ae.c:130-134`: the second declarator of
// `T *a, *b = ...;` is a variable too.
#[test]
fn test_c_second_declarator_types_field_call() {
    let g = parse(
        "ae.c",
        b"typedef struct ev { void (*fin)(int); struct ev *next; } ev;\nvoid run(ev *head) {\n    ev *next_te, *te = head;\n    te->fin(1);\n}\n",
    );
    assert_eq!(
        call_meta_rows(&g)
            .iter()
            .map(|r| (r.0, r.4))
            .collect::<Vec<_>>(),
        [("run", "ev")]
    );
}

/// A guard the `#ifndef` pairing finds (`_HH`) is no import either. A
/// `#define X 1` alone in its `#ifndef X` group sets a default, so it keeps
/// its Macro node; imports also drop every guard-spelt name defined as
/// empty or `1`, so it has no import.
#[test]
fn test_c_guard_pairing_drops_import_and_lone_default_keeps_macro() {
    let g = parse(
        "cfg.c",
        b"#ifndef FOO_HH\n#define FOO_HH\nint x;\n#endif\n#ifndef HAVE_STDINT_H\n#define HAVE_STDINT_H 1\n#endif\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Macro]),
        [("HAVE_STDINT_H", NodeKind::Macro, (5, 0, 6, 0), true, None)]
    );
    assert_eq!(import_rows(&g), []);
}

/// Declarations in an `extern "C"` block, the usual header wrapper, stay
/// at file scope.
#[test]
fn test_c_extern_c_block_decls_emit_variable_and_function() {
    let g = parse(
        "ext.c",
        b"#ifdef __cplusplus\nextern \"C\" {\n#endif\nint g_count;\nvoid api(void);\n#ifdef __cplusplus\n}\n#endif\n",
    );
    let names: Vec<_> = rows_of(&g, &[NodeKind::Variable, NodeKind::Function])
        .into_iter()
        .map(|r| (r.0, r.1))
        .collect();
    assert_eq!(
        names,
        [("g_count", NodeKind::Variable), ("api", NodeKind::Function)]
    );
}

/// `typedef struct node Node, *NodePtr;` is one `Typedef` node per
/// declarator, each holding the shared type plus its own pointer prefix
/// (no import).
#[test]
fn test_c_multi_declarator_typedef_binds_each_alias() {
    let g = parse("td.c", b"typedef struct node Node, *NodePtr;\n");
    assert_eq!(import_rows(&g), []);
    assert_eq!(
        typedef_types(&g),
        [
            ("Node", Some("struct node")),
            ("NodePtr", Some("struct node *")),
        ]
    );
}

/// A digit separator or a multi-line raw string in an error-recovered
/// file leaves later file-scope declarations at file scope.
#[test]
fn test_c_digit_separator_in_recovered_tree_keeps_later_global() {
    let g = parse(
        "sep.c",
        b"int @bad;\nvoid f(int n) {\n  if (n > 1'000) {\n    n++;\n  }\n}\nint after_global = 0;\n",
    );
    let names: Vec<_> = rows_of(&g, &[NodeKind::Variable])
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert!(names.contains(&"after_global"), "{names:?}");
}

/// FU-2026-10-30-cb159240606e: a file-scope `static` variable is private to
/// its translation unit like a `static` function, so neither is exported;
/// a plain global, a prototype and an `extern` stay exported.
#[test]
fn test_c_static_variable_is_not_exported_like_static_function() {
    let g = parse(
        "linkage.c",
        b"static int sv = 3;\nint gv;\nstatic int sf(void) { return sv; }\nint gf(void);\nextern int ev;\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, DECL_KINDS),
        [
            (
                "sv",
                NodeKind::Variable,
                (0, 0, 0, 18),
                false,
                s("static int")
            ),
            ("gv", NodeKind::Variable, (1, 0, 1, 7), true, s("int")),
            ("sf", NodeKind::Function, (2, 0, 2, 34), false, s("int")),
            ("gf", NodeKind::Function, (3, 0, 3, 13), true, None),
            (
                "ev",
                NodeKind::Variable,
                (4, 0, 4, 14),
                true,
                s("extern int")
            ),
        ]
    );
}

/// FU-2026-10-30-cb159240606e: lua spells its guards in lower case
/// (`lapi_h`). A paired guard is neither a Macro node nor an import, and a
/// lone guard-spelt `#define` is no import, in any case of the suffix.
#[test]
fn test_c_lowercase_include_guard_emits_no_macro_or_binding() {
    let g = parse(
        "lapi.c",
        b"#ifndef lapi_h\n#define lapi_h\n#define LAPI_X 2\n#endif\n#define lstate_h\n",
    );
    assert_eq!(macro_names(&g), ["LAPI_X", "lstate_h"]);
    assert_eq!(
        import_rows(&g),
        [binding("LAPI_X", "2", BindingKind::Constant)]
    );
}

/// FU-2026-10-30-9adc33b1cf3e: one declaration gets one kind in a `.c`
/// (C provider) and a `.h` (C++ provider). `const` on the object makes a
/// Const (`T *const x[]`, `const int N`); `const T *p` points at constants
/// and `p` stays a Variable.
#[test]
fn test_c_const_object_declarations_match_header_kinds() {
    let src = b"const char *const names[] = {\"a\"};\nconst char *p;\nstatic const int N = 5;\nint *const q = 0;\n";
    // `is_exported`: a `static` object has internal linkage in C and C++
    // alike, so a `.h` file exports it no more than a `.c` file does.
    let kinds = |g: &LocalGraph| -> Vec<(String, NodeKind, bool)> {
        let mut rows: Vec<(String, NodeKind, bool)> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Variable | NodeKind::Const))
            .map(|n| (n.name.clone(), n.kind, n.is_exported))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    let c = kinds(&parse("names.c", src));
    let header = kinds(
        &CppProvider::new()
            .expect("CppProvider::new")
            .parse_file(Path::new("names.h"), src)
            .expect("parse_file"),
    );
    let expected = [
        ("N".to_string(), NodeKind::Const, false),
        ("names".to_string(), NodeKind::Const, true),
        ("p".to_string(), NodeKind::Variable, true),
        ("q".to_string(), NodeKind::Const, true),
    ];
    assert_eq!(c, expected);
    assert_eq!(header, expected);
}

/// FU-2026-10-30-520106d77de4 (redis `module.c:15317`): `dictForEach(d,
/// struct RedisModule, module, BODY)` declares `struct RedisModule *module`
/// for `BODY`, so the call through its function-pointer field is a
/// dynamic dispatch on `struct RedisModule`. Outside the macro call the
/// name types nothing.
#[test]
fn test_c_macro_bound_struct_variable_types_dispatch_in_macro_body() {
    let g = parse(
        "module.c",
        b"struct RedisModule { void (*defrag_start_cb)(int *ctx); };\nvoid moduleDefragStart(void) {\n    dictForEach(modules, struct RedisModule, module,\n        if (module->defrag_start_cb) {\n            module->defrag_start_cb(&ctx);\n        }\n    );\n}\nvoid plain(void) { module->defrag_start_cb(0); }\n",
    );
    let rows: Vec<(&str, u8, &str)> = call_meta_rows(&g)
        .into_iter()
        .map(|(caller, _, _, flags, ty)| (caller, flags, ty))
        .collect();
    assert_eq!(
        rows,
        [(
            "moduleDefragStart",
            CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH,
            "struct RedisModule"
        )]
    );
}

/// FU-2026-10-30-03b1f2768b78 (jemalloc `prof_data.c:95-112`): an
/// `rb_gen(...)` call with no `;` lets an ERROR swallow the next
/// function's head and `{`, so its locals sit under the root. In a tree
/// with errors the source's brace depth decides: they are no Variables.
/// The functions around stay.
#[test]
fn test_c_locals_after_unterminated_macro_call_emit_no_variable() {
    let g = parse(
        "prof_data.c",
        b"int before(int a) { return a; }\nrb_gen(static UNUSED, gctx_tree_, prof_gctx_tree_t, prof_gctx_t, dump_link,\n    prof_gctx_comp)\n    /* NOLINTEND(performance-no-int-to-ptr) */\n\n    static int prof_tdata_comp(const prof_tdata_t *a, const prof_tdata_t *b) {\n\tint      ret;\n\tuint64_t a_uid = a->thr_uid;\n\tret = (a_uid > 1);\n\treturn ret;\n}\nint after(int b) { return b; }\nint global_after;\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable]),
        [(
            "global_after",
            NodeKind::Variable,
            (12, 0, 12, 17),
            true,
            s("int")
        )]
    );
    let functions: Vec<&str> = rows_of(&g, &[NodeKind::Function])
        .iter()
        .map(|r| r.0)
        .collect();
    assert!(
        functions.contains(&"before") && functions.contains(&"after"),
        "{functions:?}"
    );
}

/// FU-2026-10-30-03b1f2768b78 (jemalloc `test/unit/rb.c:1000`): after a
/// `TEST_END` with no `;`, tree-sitter-c reads `TEST_END typedef` as one
/// declaration and `rb_tree(node_t) tree_t;` as the next. The keyword
/// makes the second a Typedef from `typedef` to its `;`, no Variable.
#[test]
fn test_c_typedef_split_by_preceding_macro_emits_typedef_not_variable() {
    let g = parse(
        "rb.c",
        b"TEST_BEGIN(test_rb_update_search) {\n\trun(2);\n}\nTEST_END\n\ntypedef rb_tree(node_t) unsummarized_tree_t;\nrb_gen(static UNUSED, unsummarized_tree_, unsummarized_tree_t, node_t, link,\n    node_cmp);\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef, NodeKind::Variable]),
        [(
            "unsummarized_tree_t",
            NodeKind::Typedef,
            (5, 0, 5, 44),
            true,
            None
        )]
    );
    assert_eq!(
        named_content_hash(&g, "unsummarized_tree_t"),
        xxh3_64_bytes(b"typedef rb_tree(node_t) unsummarized_tree_t;")
    );
}

/// FU-2026-10-30-03b1f2768b78 (jemalloc `jemalloc.c:35`): an attribute
/// macro in a conditional between a global and its `;` leaves the
/// declaration with a missing `;` and the `;` as an empty statement. The
/// global is a Variable; the next global is unchanged.
#[test]
fn test_c_global_with_attribute_in_conditional_before_semicolon_emits_variable() {
    let g = parse(
        "jemalloc.c",
        b"const char *je_malloc_conf\n#ifndef _WIN32\n    JEMALLOC_ATTR(weak)\n#endif\n        ;\nconst char *opt_symlink = NULL;\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable, NodeKind::Const]),
        [
            (
                "je_malloc_conf",
                NodeKind::Variable,
                (0, 0, 0, 26),
                true,
                s("const char *")
            ),
            (
                "opt_symlink",
                NodeKind::Variable,
                (5, 0, 5, 31),
                true,
                s("const char *")
            ),
        ]
    );
}

/// FU-2026-10-30-03b1f2768b78 (jemalloc `jemalloc.c:1210`): a prototype
/// with a non-pointer return type and a trailing macro (`void
/// __libc_free(void *ptr) PREALIAS(je_free);`) parses as an ERROR before
/// an empty `;` statement. It is a Function like the pointer-returning
/// one, spanning its `;`, with no type as a prototype has none.
#[test]
fn test_c_prototype_with_trailing_macro_emits_function() {
    let g = parse(
        "jemalloc.c",
        b"#define PREALIAS(je_fn) ALIAS(je_fn)\nvoid *__libc_calloc(size_t n, size_t size) PREALIAS(je_calloc);\nvoid __libc_free(void *ptr) PREALIAS(je_free);\nint __posix_memalign(void **r, size_t a, size_t s) PREALIAS(je_posix_memalign);\n",
    );
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Function]),
        [
            (
                "__libc_calloc",
                NodeKind::Function,
                (1, 0, 1, 63),
                true,
                None
            ),
            ("__libc_free", NodeKind::Function, (2, 0, 2, 46), true, None),
            (
                "__posix_memalign",
                NodeKind::Function,
                (3, 0, 3, 79),
                true,
                None
            ),
        ]
    );
}

// ---- Review fixes (C/C++ batch A) ----

/// The counterexample to FU-2026-10-30-520106d77de4's rule: a macro call's
/// `struct A, b` pair declares no `b` when the function already binds `b`
/// (the parameter in `f`, the earlier local in `g`), and declares nothing
/// when no statement follows the pair (`h`, an expression-only macro). The
/// dispatch type is the type the call really dispatches on.
#[test]
fn test_declare_macro_bound_vars_existing_binding_keeps_its_type() {
    let g = parse(
        "with_type.c",
        b"#define WITH_TYPE(x, type, ptr, expr) (expr)\n#define LOOP(d, type, var, ...) do { __VA_ARGS__ } while (0)\nstruct A { void (*cb)(void); };\nstruct B { void (*cb)(void); };\nvoid f(struct B *b) { WITH_TYPE(0, struct A, b, b->cb()); }\nvoid g(void) {\n    struct B *b = 0;\n    LOOP(d, struct A, b, b->cb(););\n}\nvoid h(void) { WITH_TYPE(0, struct A, a, a->cb()); }\n",
    );
    let rows: Vec<(&str, u8, &str)> = call_meta_rows(&g)
        .into_iter()
        .map(|(caller, _, _, flags, ty)| (caller, flags, ty))
        .collect();
    let dynamic = CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH;
    assert_eq!(
        rows,
        [("f", dynamic, "struct B"), ("g", dynamic, "struct B")]
    );
}

/// After a typedef cut short at an attribute, the tail declaration names
/// the alias through its array or pointer declarator and past a qualifier
/// (`float vec4[4]`, `T *tp`, `const T ct`). Each is a Typedef spanning
/// both statements whose `type_annotation` keeps the pointer stars (no
/// import); none is a Variable.
#[test]
fn test_c_attribute_typedef_tail_declarator_shapes_emit_typedef() {
    let g = parse(
        "vec.c",
        b"typedef __attribute__((__aligned__(16))) float vec4[4];\ntypedef __attribute__((__aligned__(8))) T *tp;\ntypedef __attribute__((__aligned__(8))) const T ct;\n",
    );
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Typedef, NodeKind::Variable]),
        [
            (
                "vec4",
                NodeKind::Typedef,
                (0, 0, 0, 55),
                true,
                Some("float")
            ),
            ("tp", NodeKind::Typedef, (1, 0, 1, 46), true, Some("T *")),
            ("ct", NodeKind::Typedef, (2, 0, 2, 51), true, Some("T")),
        ]
    );
    assert_eq!(import_rows(&g), []);
}

/// The C binding filter and the preprocessor guard detection read one
/// guard spelling: `_HXX` / `_HH` endings and a trailing `_` are guard
/// names, so a guard-spelt `#define` binds no name. A real constant stays.
#[test]
fn test_is_include_guard_hxx_hh_and_trailing_underscore_emit_no_binding() {
    let g = parse(
        "guards.c",
        b"#define FOO_HXX\n#define BAR_HH 1\n#define BAZ_H_\n#define LIMIT 4096\n",
    );
    let names: Vec<&str> = import_rows(&g).iter().map(|r| r.1).collect();
    assert_eq!(names, ["LIMIT"]);
}
