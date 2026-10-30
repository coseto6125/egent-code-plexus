//! `CProvider` defects found in the driver-move review (FU-2026-10-30-
//! 8f38f58dd41a, plus the multi-declarator type sibling of
//! FU-2026-10-30-955135c9e46b). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::c::parser::CProvider;
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
            ("lua_Reader", NodeKind::Typedef, (0, 0, 0, 72), true, None),
            (
                "RedisModuleTypeLoadFunc",
                NodeKind::Typedef,
                (1, 0, 1, 73),
                true,
                None
            ),
            (
                "getClientChannelsFn",
                NodeKind::Typedef,
                (2, 0, 2, 47),
                true,
                None
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
/// prototype `alloc` keep today's records.
#[test]
fn test_c_nested_fn_ptr_and_triple_pointer_typedefs_emit_typedef_nodes() {
    let g = parse(
        "typedefs2.c",
        b"typedef char ***str_cube;\ntypedef int (*handlers[4])(int);\ntypedef int (**handler_pp)(int);\ntypedef char **(*argv_fn)(void);\n#ifdef HAVE_ALLOC\ntypedef void *(*alloc_fn)(size_t);\n#endif\ntypedef void *(*)(int);\ntypedef int (*cmp_fn)(int, int);\ntypedef unsigned long counter_t;\nvoid *alloc(size_t n);\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef, NodeKind::Function]),
        [
            ("str_cube", NodeKind::Typedef, (0, 0, 0, 25), true, None),
            ("handlers", NodeKind::Typedef, (1, 0, 1, 32), true, None),
            ("handler_pp", NodeKind::Typedef, (2, 0, 2, 32), true, None),
            ("argv_fn", NodeKind::Typedef, (3, 0, 3, 32), true, None),
            ("alloc_fn", NodeKind::Typedef, (5, 0, 5, 34), true, None),
            ("cmp_fn", NodeKind::Typedef, (8, 0, 8, 32), true, None),
            ("counter_t", NodeKind::Typedef, (9, 0, 9, 32), true, None),
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
                true,
                s("static wchar_t")
            ),
            (
                "woffs",
                NodeKind::Variable,
                (2, 0, 2, 29),
                true,
                s("static int")
            ),
            (
                "ncleanups",
                NodeKind::Variable,
                (4, 0, 4, 38),
                true,
                s("static unsigned")
            ),
            (
                "cleanups",
                NodeKind::Variable,
                (5, 0, 5, 62),
                true,
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
/// an array beside a scalar in one declaration are Variables. A `static`
/// array inside a body is a local and a struct array member stays a
/// Property.
#[test]
fn test_c_initialised_pointer_and_2d_arrays_emit_variables_but_local_does_not() {
    let g = parse(
        "arrays.c",
        b"static const uint8_t bitsinbyte[256] = {0};\nstatic char *bio_worker_title[] = {\"a\", \"b\"};\nstatic uint64_t combine_cache[64][64];\nint pair[2], single;\nstruct S { int buf[8]; };\nvoid f(void) { static int local_arr[4]; }\n",
    );
    let s = Some;
    assert_eq!(
        sorted_rows_of(&g, &[NodeKind::Variable, NodeKind::Property]),
        [
            (
                "bitsinbyte",
                NodeKind::Variable,
                (0, 0, 0, 43),
                true,
                s("static const uint8_t")
            ),
            (
                "bio_worker_title",
                NodeKind::Variable,
                (1, 0, 1, 45),
                true,
                s("static char *")
            ),
            (
                "combine_cache",
                NodeKind::Variable,
                (2, 0, 2, 38),
                true,
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
                true,
                s("static int")
            ),
            (
                "plain",
                NodeKind::Variable,
                (25, 0, 25, 17),
                true,
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
                true,
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
                true,
                s("static int")
            ),
            (
                "close_text",
                NodeKind::Variable,
                (12, 0, 12, 37),
                true,
                s("static const char *")
            ),
            (
                "legacy_gate",
                NodeKind::Variable,
                (16, 0, 16, 23),
                true,
                s("static int")
            ),
            (
                "modern_gate",
                NodeKind::Variable,
                (18, 0, 18, 23),
                true,
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
            ("handler_fn", NodeKind::Typedef, (0, 0, 0, 29), true, None),
            ("names_t", NodeKind::Typedef, (1, 0, 1, 25), true, None),
        ]
    );
}

/// Off-task 2: the leading stars of a typedef declarator belong to the
/// returned or element type, so the import source keeps them
/// (`void *`, `char **`, `const char *`, `char *`). A function-pointer
/// typedef with a plain return type and a data-pointer typedef keep
/// today's sources.
#[test]
fn test_c_pointer_return_typedef_import_source_keeps_return_stars() {
    let g = parse(
        "returns.c",
        b"typedef void *(*alloc_fn)(size_t);\ntypedef char **(*argv_fn)(void);\ntypedef const char * (*lua_Reader) (lua_State *L);\ntypedef char *names_t[4];\ntypedef int (*cmp_fn)(int);\ntypedef char **str_arr;\n",
    );
    assert_eq!(
        import_rows(&g),
        [
            binding("alloc_fn", "void *", BindingKind::Alias),
            binding("argv_fn", "char **", BindingKind::Alias),
            binding("lua_Reader", "const char *", BindingKind::Alias),
            binding("names_t", "char *", BindingKind::Alias),
            binding("cmp_fn", "int", BindingKind::Alias),
            binding("str_arr", "char **", BindingKind::Alias),
        ]
    );
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
            true,
            Some("static int")
        )]
    );
}

/// A-R3, C side (`deps/xxhash/xxhash.h:2655`): both grammars read
/// `typedef __attribute__((aligned(1))) T x;` as a typedef cut short at the
/// attribute, with a missing `;`. Its parenthesized declarator holds the
/// attribute's argument, so neither a Typedef nor a named binding takes the
/// name `__aligned__`. A macro-call type stays a Typedef with its alias.
#[test]
fn test_c_attribute_typedef_emits_no_attribute_named_typedef() {
    let g = parse(
        "witness.c",
        b"typedef ql_head(witness_t) witness_list_t;\ntypedef __attribute__((__aligned__(1))) unsigned int xxh_unalign32;\n",
    );
    assert_eq!(
        rows_of(&g, &[NodeKind::Typedef]),
        [(
            "witness_list_t",
            NodeKind::Typedef,
            (0, 0, 0, 42),
            true,
            None
        )]
    );
    let aliases: Vec<&str> = import_rows(&g).iter().map(|r| r.1).collect();
    assert_eq!(aliases, ["witness_list_t"]);
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
                true,
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
