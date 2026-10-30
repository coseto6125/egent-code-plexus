//! `CppProvider` defects found in the driver-move review
//! (FU-2026-10-30-955135c9e46b). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{CallMeta, NodeKind};
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span = (u32, u32, u32, u32);

/// `(name, kind, span, owner_class, type_annotation)`
type Row<'g> = (&'g str, NodeKind, Span, Option<&'g str>, Option<&'g str>);

fn parse(path: &str, src: &[u8]) -> LocalGraph {
    CppProvider::new()
        .expect("CppProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

/// Every node, ordered by span then name: the lexical recovery appends
/// nodes, so emitted order is no contract here.
fn sorted_rows(g: &LocalGraph) -> Vec<Row<'_>> {
    let mut rows: Vec<Row<'_>> = g
        .nodes
        .iter()
        .map(|n| {
            (
                n.name.as_str(),
                n.kind,
                n.span,
                n.owner_class.as_deref(),
                n.type_annotation.as_deref(),
            )
        })
        .collect();
    rows.sort_by(|a, b| (a.2, a.0).cmp(&(b.2, b.0)));
    rows
}

fn named<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node `{name}` in {:?}", sorted_rows(g)))
}

fn contains(outer: Span, inner: Span) -> bool {
    (outer.0, outer.1) <= (inner.0, inner.1) && (inner.2, inner.3) <= (outer.2, outer.3)
}

/// The owner of the one `<anonymous>` lambda inside `function`'s span.
fn lambda_owner_in<'g>(g: &'g LocalGraph, function: &str) -> Option<&'g str> {
    let outer = named(g, function).span;
    let lambdas: Vec<&RawNode> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("<anonymous") && contains(outer, n.span))
        .collect();
    assert_eq!(lambdas.len(), 1, "one lambda in `{function}`: {lambdas:?}");
    lambdas[0].owner_class.as_deref()
}

/// `#if` / `#else` each open the initializer list with `:`. The class keeps
/// its constructor, the next method keeps its own span and kind, and the
/// data member stays a Property of the class.
#[test]
fn test_cpp_if_else_in_ctor_init_list_keeps_class_members() {
    let g = parse(
        "holder.cpp",
        b"class Holder {\npublic:\n  Holder()\n#if FAST\n    : a(1)\n#else\n    : a(2)\n#endif\n  {}\n  void keep() { touch(); }\n  int a;\n};\n",
    );
    let holder = Some("Holder");
    assert_eq!(
        sorted_rows(&g),
        [
            ("Holder", NodeKind::Class, (0, 0, 11, 1), None, None),
            ("Holder", NodeKind::Constructor, (2, 2, 8, 4), holder, None),
            (
                "keep",
                NodeKind::Method,
                (9, 2, 9, 26),
                holder,
                Some("void")
            ),
            ("a", NodeKind::Property, (10, 2, 10, 8), holder, Some("int")),
        ]
    );
    let keep = named(&g, "keep");
    assert_eq!(
        keep.content_hash,
        xxh3_64_bytes(b"void keep() { touch(); }")
    );
    assert_eq!(keep.calls, ["touch"]);
}

/// Atypical: a template class with a base, both branches ending in `,`, and
/// the list continuing after `#endif`.
#[test]
fn test_cpp_if_else_trailing_comma_init_list_in_template_class_keeps_members() {
    let g = parse(
        "wrap.cpp",
        b"template<typename T>\nclass Wrap : public Base<T> {\npublic:\n  Wrap()\n#if A\n    : x(1),\n#else\n    : x(2),\n#endif\n      y(3) {}\n  void touch() { go(); }\n  int x;\n  int y;\n};\n",
    );
    let wrap = Some("Wrap");
    assert_eq!(
        sorted_rows(&g),
        [
            ("Wrap", NodeKind::Class, (1, 0, 13, 1), None, None),
            ("Wrap", NodeKind::Constructor, (3, 2, 9, 13), wrap, None),
            (
                "touch",
                NodeKind::Method,
                (10, 2, 10, 24),
                wrap,
                Some("void")
            ),
            ("x", NodeKind::Property, (11, 2, 11, 8), wrap, Some("int")),
            ("y", NodeKind::Property, (12, 2, 12, 8), wrap, Some("int")),
        ]
    );
}

/// Atypical: the conditional wraps the whole initializer list, so the
/// constructor declarator and its `:` sit on opposite sides of `#if`. The
/// constructor is a node like the unconditional one.
#[test]
fn test_cpp_if_around_whole_init_list_keeps_constructor() {
    let g = parse(
        "conn.cpp",
        b"class Conn {\npublic:\n    Conn(int fd)\n#if USE_TLS\n        : fd_(fd), tls_(nullptr)\n#else\n        : fd_(fd)\n#endif\n    {}\n    int read(char *buf) { return fd_; }\n    int fd_;\n};\n",
    );
    let conn = Some("Conn");
    assert_eq!(
        sorted_rows(&g),
        [
            ("Conn", NodeKind::Class, (0, 0, 11, 1), None, None),
            ("Conn", NodeKind::Constructor, (2, 4, 8, 6), conn, None),
            ("read", NodeKind::Method, (9, 4, 9, 39), conn, Some("int")),
            (
                "fd_",
                NodeKind::Property,
                (10, 4, 10, 12),
                conn,
                Some("int")
            ),
        ]
    );
}

/// Guard: an `#if` after the first initializer already parses; its records
/// stay as they are.
#[test]
fn test_cpp_if_after_first_initializer_records_unchanged() {
    let g = parse(
        "conn.cpp",
        b"class Conn {\npublic:\n    Conn(int fd)\n        : fd_(fd)\n#if USE_TLS\n        , tls_(nullptr)\n#endif\n    {}\n    int read(char *buf);\n    int fd_;\n};\n",
    );
    let conn = Some("Conn");
    assert_eq!(
        sorted_rows(&g),
        [
            ("Conn", NodeKind::Class, (0, 0, 10, 1), None, None),
            ("Conn", NodeKind::Constructor, (2, 4, 7, 6), conn, None),
            ("read", NodeKind::Method, (8, 4, 8, 24), conn, Some("int")),
            ("fd_", NodeKind::Property, (9, 4, 9, 12), conn, Some("int")),
        ]
    );
}

/// `int x, y;` declares two `int`s: each declarator's type is the shared
/// specifier plus its own pointer prefix, never the declarator before it.
/// Span and hash stay the whole declaration's, as in Java and C#.
#[test]
fn test_cpp_multi_declarator_types_exclude_previous_declarator() {
    let g = parse(
        "point.cpp",
        b"struct Point {\n    int x, y;\n    int *p, q;\n    char *a, *b;\n};\nint gx, gy;\n",
    );
    let point = Some("Point");
    let s = Some;
    assert_eq!(
        sorted_rows(&g),
        [
            ("Point", NodeKind::Struct, (0, 0, 4, 1), None, None),
            ("x", NodeKind::Property, (1, 4, 1, 13), point, s("int")),
            ("y", NodeKind::Property, (1, 4, 1, 13), point, s("int")),
            ("p", NodeKind::Property, (2, 4, 2, 14), point, s("int *")),
            ("q", NodeKind::Property, (2, 4, 2, 14), point, s("int")),
            ("a", NodeKind::Property, (3, 4, 3, 16), point, s("char *")),
            ("b", NodeKind::Property, (3, 4, 3, 16), point, s("char *")),
            ("gx", NodeKind::Variable, (5, 0, 5, 11), None, s("int")),
            ("gy", NodeKind::Variable, (5, 0, 5, 11), None, s("int")),
        ]
    );
    assert_eq!(named(&g, "x").content_hash, xxh3_64_bytes(b"int x, y;"));
    assert_eq!(named(&g, "y").content_hash, xxh3_64_bytes(b"int x, y;"));
    assert_eq!(named(&g, "gy").content_hash, xxh3_64_bytes(b"int gx, gy;"));
}

/// A friend belongs to the namespace around its class, and so does a lambda
/// in its body, as for a free function. A lambda in a real member keeps the
/// class; at file scope the friend and its lambda have no owner.
#[test]
fn test_cpp_lambda_in_friend_body_takes_friend_owner_not_class() {
    let g = parse(
        "friend.cpp",
        b"namespace ns {\nclass Foo {\n    friend void run(Foo &a) { go([](){ helper(); }); }\n    void own(int a) { go([](){ helper(); }); }\n};\n}\nstruct Bar {\n    friend void bar_run(Bar &a) { go([](){ helper(); }); }\n};\n",
    );
    assert_eq!(named(&g, "run").owner_class.as_deref(), Some("ns"));
    assert_eq!(lambda_owner_in(&g, "run"), Some("ns"));
    assert_eq!(named(&g, "own").owner_class.as_deref(), Some("Foo"));
    assert_eq!(lambda_owner_in(&g, "own"), Some("Foo"));
    assert_eq!(named(&g, "bar_run").owner_class, None);
    assert_eq!(lambda_owner_in(&g, "bar_run"), None);
}

/// Sibling of the C typedef fix: an alias inside a pointer, function-pointer
/// or array declarator is a `Typedef` node like the plain one beside it.
#[test]
fn test_cpp_pointer_fn_ptr_and_array_typedefs_emit_typedef_nodes() {
    let g = parse(
        "typedefs.cpp",
        b"typedef char **str_arr;\ntypedef int (*cmp_fn)(int, int);\ntypedef int int_arr[10];\ntypedef unsigned long counter_t;\n",
    );
    assert_eq!(
        sorted_rows(&g),
        [
            ("str_arr", NodeKind::Typedef, (0, 0, 0, 23), None, None),
            ("cmp_fn", NodeKind::Typedef, (1, 0, 1, 32), None, None),
            ("int_arr", NodeKind::Typedef, (2, 0, 2, 24), None, None),
            ("counter_t", NodeKind::Typedef, (3, 0, 3, 32), None, None),
        ]
    );
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

/// `.h` files parse as C++. `#ifndef X` / `#define X` (empty body) is an
/// include guard: no Macro node, and the raw-source fallback must not
/// re-add one. The real macro inside stays.
#[test]
fn test_cpp_include_guard_define_emits_no_macro_node() {
    let g = parse(
        "api.h",
        b"#ifndef API_H\n#define API_H\n#define API_VERSION 2\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["API_VERSION"]);
}

/// Guard spellings: Google-style `_H_`, `_HPP`, and `#if !defined(X)`.
/// `#ifndef BUF_SIZE` / `#define BUF_SIZE 4096` has a body, so it is a
/// default value, not a guard: its Macro stays.
#[test]
fn test_cpp_include_guard_spellings_skipped_but_default_value_define_kept() {
    let g = parse(
        "widget.hpp",
        b"#ifndef WIDGET_H_\n#define WIDGET_H_\n#define WIDGET_MAX 8\n#ifndef BUF_SIZE\n#define BUF_SIZE 4096\n#endif\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["BUF_SIZE", "WIDGET_MAX"]);

    let g = parse(
        "legacy.hpp",
        b"#if !defined(LEGACY_HPP)\n#define LEGACY_HPP\n#define LEGACY_ON 1\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["LEGACY_ON"]);
}

/// Guard: a file-level `#ifndef` whose name is not guard-shaped is a
/// feature default, so its `#define` stays a Macro.
#[test]
fn test_cpp_non_guard_ifndef_define_is_kept() {
    let g = parse(
        "feature.h",
        b"#ifndef USE_FEATURE\n#define USE_FEATURE\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["USE_FEATURE"]);
}

/// A `#define` on the line after one ending in `\` is part of that macro's
/// body, so it defines nothing. Covers LF and CRLF.
#[test]
fn test_cpp_define_inside_continued_macro_body_emits_no_macro_node() {
    let lf = parse(
        "cont.h",
        b"#define FIRST 1\n#define OUTER(x) \\\n#define NESTED 1\n#define AFTER 2\n",
    );
    assert_eq!(macro_names(&lf), ["AFTER", "FIRST", "OUTER"]);

    let crlf = parse(
        "cont_crlf.h",
        b"#define OUTER 1 \\\r\n#define NESTED 2\r\n#define AFTER 3\r\n",
    );
    assert_eq!(macro_names(&crlf), ["AFTER", "OUTER"]);
}

/// Guard: a finished continuation does not hide the next real `#define`.
#[test]
fn test_cpp_define_after_finished_continuation_is_kept() {
    let g = parse(
        "cont_end.h",
        b"#define LONG(x) \\\n    ((x) + 1)\n#define NEXT 2\n",
    );
    assert_eq!(macro_names(&g), ["LONG", "NEXT"]);
}

// ---- Round 2 (P5 group A) ----

fn rows_of_kinds<'g>(g: &'g LocalGraph, kinds: &[NodeKind]) -> Vec<Row<'g>> {
    sorted_rows(g)
        .into_iter()
        .filter(|r| kinds.contains(&r.1))
        .collect()
}

/// A1, C++ side (`.h` parses as C++): a function-pointer typedef whose
/// return type is a pointer is a `Typedef` node (`lua.h`, `redismodule.h`).
#[test]
fn test_cpp_pointer_return_fn_ptr_typedefs_emit_typedef_nodes() {
    let g = parse(
        "lua.h",
        b"typedef const char * (*lua_Reader) (lua_State *L, void *ud, size_t *sz);\ntypedef void *(*RedisModuleTypeLoadFunc)(RedisModuleIO *rdb, int encver);\ntypedef dict *(*getClientChannelsFn)(client *);\n",
    );
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Typedef]),
        [
            ("lua_Reader", NodeKind::Typedef, (0, 0, 0, 72), None, None),
            (
                "RedisModuleTypeLoadFunc",
                NodeKind::Typedef,
                (1, 0, 1, 73),
                None,
                None
            ),
            (
                "getClientChannelsFn",
                NodeKind::Typedef,
                (2, 0, 2, 47),
                None,
                None
            ),
        ]
    );
}

/// A1 atypical, C++ side: `***`, an array of function pointers, a pointer
/// to a function pointer, a `**` return and the A1 shape inside `#ifdef`.
/// The abstract declarator names nothing; `cmp_fn` and `counter_t` stay.
#[test]
fn test_cpp_nested_fn_ptr_and_triple_pointer_typedefs_emit_typedef_nodes() {
    let g = parse(
        "typedefs2.h",
        b"typedef char ***str_cube;\ntypedef int (*handlers[4])(int);\ntypedef int (**handler_pp)(int);\ntypedef char **(*argv_fn)(void);\n#ifdef HAVE_ALLOC\ntypedef void *(*alloc_fn)(size_t);\n#endif\ntypedef void *(*)(int);\ntypedef int (*cmp_fn)(int, int);\ntypedef unsigned long counter_t;\n",
    );
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Typedef]),
        [
            ("str_cube", NodeKind::Typedef, (0, 0, 0, 25), None, None),
            ("handlers", NodeKind::Typedef, (1, 0, 1, 32), None, None),
            ("handler_pp", NodeKind::Typedef, (2, 0, 2, 32), None, None),
            ("argv_fn", NodeKind::Typedef, (3, 0, 3, 32), None, None),
            ("alloc_fn", NodeKind::Typedef, (5, 0, 5, 34), None, None),
            ("cmp_fn", NodeKind::Typedef, (8, 0, 8, 32), None, None),
            ("counter_t", NodeKind::Typedef, (9, 0, 9, 32), None, None),
        ]
    );
    assert!(
        g.nodes.iter().all(|n| !n.name.is_empty()),
        "the abstract declarator names nothing: {:?}",
        sorted_rows(&g)
    );
}

/// A2: the file-level guard decision reads the source, not the recovered
/// tree. The `#if` / `#else` branches each open a brace, so the guard lands
/// in an ERROR node (`libuv.h`, `atomic.h`, `rb.h`, `json.hpp`); its
/// `#define` still emits no Macro.
#[test]
fn test_cpp_include_guard_in_error_recovered_tree_emits_no_macro_node() {
    let g = parse(
        "libuv.h",
        b"#ifndef __HIREDIS_LIBUV_H__\n#define __HIREDIS_LIBUV_H__\n#define LIBUV_KEEP 1\nint f() {\n#if A\n  if (a) {\n#else\n  if (b) {\n#endif\n  }\n}\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["LIBUV_KEEP"]);
}

/// A2 (`doctest.h` shape): the file-level guard closes before further
/// top-level blocks, and the tree is error-recovered. The guard is still at
/// conditional depth 0, so its `#define` emits no Macro, as `SECOND_H` in
/// the clean-tree guard test below.
#[test]
fn test_cpp_guard_before_trailing_block_in_recovered_tree_emits_no_macro_node() {
    let g = parse(
        "doctest.h",
        b"#ifndef DOCTEST_LIBRARY_INCLUDED\n#define DOCTEST_LIBRARY_INCLUDED\n#define DOCTEST_VERSION_MAJOR 2\nint f() {\n#if A\n  if (a) {\n#else\n  if (b) {\n#endif\n  }\n}\n#endif // DOCTEST_LIBRARY_INCLUDED\n#ifdef DOCTEST_UNDEF_NOMINMAX\n#undef NOMINMAX\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["DOCTEST_VERSION_MAJOR"]);
}

/// A2: a guard nested inside the file-level guard keeps its Macro, also
/// when error recovery closes the outer guard early and lifts the nested
/// one to the top of the tree (`tests/abi/.../json_v3_10_5.hpp:3288`; the
/// same guard is kept in `single_include/nlohmann/json.hpp:3506`).
#[test]
fn test_cpp_nested_guard_lifted_by_error_recovery_keeps_macro_node() {
    let g = parse(
        "json_v3_10_5.hpp",
        b"#ifndef INCLUDE_NLOHMANN_JSON_HPP_\n#define INCLUDE_NLOHMANN_JSON_HPP_\nint pick() {\n#if A\n  return 1; }\n#else\n  return 2; }\n#endif\n#ifndef INCLUDE_NLOHMANN_JSON_FWD_HPP_\n#define INCLUDE_NLOHMANN_JSON_FWD_HPP_\nint fwd;\n#endif\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["INCLUDE_NLOHMANN_JSON_FWD_HPP_"]);
    assert_eq!(
        named(&g, "INCLUDE_NLOHMANN_JSON_FWD_HPP_").span,
        (9, 0, 10, 0)
    );
}

/// A2 guard: on a clean tree, two nested guards side by side keep their
/// Macros, two file-level guards side by side lose theirs, and a `#define`
/// whose name differs from the `#ifndef` is no guard.
#[test]
fn test_cpp_guard_neighbours_on_clean_tree_unchanged() {
    let g = parse(
        "guards.h",
        b"#ifndef OUTER_H\n#define OUTER_H\n#ifndef INNER_H\n#define INNER_H\n#endif\n#ifndef PEER_H\n#define PEER_H\n#endif\n#endif\n#ifndef SECOND_H\n#define SECOND_H\n#endif\n#ifndef WRONG_H\n#define OTHER_H\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["INNER_H", "OTHER_H", "PEER_H"]);
}

/// A2 edge: an empty header and a guard with no name emit no Macro.
#[test]
fn test_cpp_empty_file_and_nameless_guard_emit_no_macro_node() {
    assert!(parse("empty.h", b"").nodes.is_empty());
    let g = parse("noname.h", b"#ifndef\n#define\n#endif\n");
    assert!(macro_names(&g).is_empty(), "{:?}", sorted_rows(&g));
}

/// `(caller_name, caller_span, flags, dispatch_type)`. `call_index` is left
/// out: receiver typing may rewrite the callee text it indexes.
type CallMetaRow<'g> = (&'g str, Span, u8, &'g str);

fn call_meta_rows(g: &LocalGraph) -> Vec<CallMetaRow<'_>> {
    g.call_metas
        .iter()
        .map(|m| {
            (
                m.caller_name.as_str(),
                m.caller_span,
                m.flags,
                m.dispatch_type.as_str(),
            )
        })
        .collect()
}

/// A3: a typed parameter types the receiver calls of its own function only.
/// `b`'s `auto val` and the lambda's `auto val` are bindings the binder
/// cannot type, so their calls carry no dispatch type; `c`'s own call keeps
/// `c`'s parameter type.
#[test]
fn test_cpp_param_type_does_not_leak_into_same_name_in_other_scope() {
    let g = parse(
        "handlers.cpp",
        b"struct Handler { void run(); };\nvoid a(Handler *val) { val->run(); }\nvoid b() { auto val = make(); val->run(); }\nvoid c(Handler *val) { auto f = [](auto val) { val->run(); }; val->run(); }\n",
    );
    let dynamic = CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH;
    assert_eq!(
        call_meta_rows(&g),
        [
            ("a", (1, 0, 1, 36), dynamic, "Handler"),
            ("c", (3, 0, 3, 75), dynamic, "Handler"),
        ]
    );
}

/// A3 (`include/nlohmann/json.hpp:836-906`): two constructors name their
/// parameter `val` with different types. Each constructor's call through
/// `val` carries its own parameter's type, never the other one's.
#[test]
fn test_cpp_same_name_params_of_two_ctors_keep_own_dispatch_type() {
    let g = parse(
        "json.hpp",
        b"class basic_json {\n  public:\n    template < typename CompatibleType >\n    basic_json(CompatibleType && val)\n    {\n        val.get();\n    }\n    template < typename BasicJsonType >\n    basic_json(const BasicJsonType& val)\n    {\n        val.type();\n    }\n};\n",
    );
    let mut ctors: Vec<Span> = g
        .nodes
        .iter()
        .filter(|n| n.name == "basic_json" && n.kind != NodeKind::Class)
        .map(|n| n.span)
        .collect();
    ctors.sort_unstable();
    assert_eq!(ctors.len(), 2, "two constructors: {:?}", sorted_rows(&g));
    let dynamic = CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH;
    assert_eq!(
        call_meta_rows(&g),
        [
            ("basic_json", ctors[0], dynamic, "CompatibleType"),
            ("basic_json", ctors[1], dynamic, "BasicJsonType"),
        ]
    );
}

/// A4, C++ side: file-scope arrays are Variables (a `const` one a Const),
/// like the scalar beside them. A `static` local array and a struct array
/// member are not Variables.
#[test]
fn test_cpp_file_scope_array_decls_emit_variable_nodes() {
    let g = parse(
        "arrays.cpp",
        b"static const uint8_t bitsinbyte[256] = {0};\nstatic char *bio_worker_title[] = {\"a\", \"b\"};\nstatic uint64_t combine_cache[64][64];\nint pair[2], single;\nstruct S { int buf[8]; };\nvoid f(void) { static int local_arr[4]; }\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Variable, NodeKind::Const]),
        [
            (
                "bitsinbyte",
                NodeKind::Const,
                (0, 0, 0, 43),
                None,
                s("static const uint8_t")
            ),
            (
                "bio_worker_title",
                NodeKind::Variable,
                (1, 0, 1, 45),
                None,
                s("static char *")
            ),
            (
                "combine_cache",
                NodeKind::Variable,
                (2, 0, 2, 38),
                None,
                s("static uint64_t")
            ),
            ("pair", NodeKind::Variable, (3, 0, 3, 20), None, s("int")),
            ("single", NodeKind::Variable, (3, 0, 3, 20), None, s("int")),
        ]
    );
    assert_eq!(
        named(&g, "combine_cache").content_hash,
        xxh3_64_bytes(b"static uint64_t combine_cache[64][64];")
    );
}

// ---- Round 2 phase B: siblings and off-task fixes ----

/// A1 sibling: a reference typedef, a function-type typedef and an
/// array-of-pointers typedef are `Typedef` nodes too.
#[test]
fn test_cpp_reference_and_function_type_typedefs_emit_typedef_nodes() {
    let g = parse(
        "aliases.h",
        b"typedef int &int_ref;\ntypedef void handler_fn(int);\ntypedef char *names_t[4];\n",
    );
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Typedef]),
        [
            ("int_ref", NodeKind::Typedef, (0, 0, 0, 21), None, None),
            ("handler_fn", NodeKind::Typedef, (1, 0, 1, 29), None, None),
            ("names_t", NodeKind::Typedef, (2, 0, 2, 25), None, None),
        ]
    );
}

/// A2 atypical, C++ side: comments around the guard leave it a guard; a
/// `#if` inside a block comment is no conditional.
#[test]
fn test_cpp_include_guard_around_comments_emits_no_macro_node() {
    let g = parse(
        "cfg.h",
        b"/* header\n#if FAKE\n*/\n#ifndef CFG_H // guard\n/* note */\n#define CFG_H 1\n#define CFG_ON 1\n#endif\n",
    );
    assert_eq!(macro_names(&g), ["CFG_ON"]);
}

/// Off-task 1: a C++ file-scope variable inside an include guard, a
/// namespace or an `extern "C"` block is a Variable (a `const` one a
/// Const), owned by its innermost named namespace. A function-body local
/// stays out and a class data member stays a Property of its class.
#[test]
fn test_cpp_guarded_namespace_and_extern_c_variables_emit_variable_nodes() {
    let g = parse(
        "api.h",
        b"#ifndef API_H\n#define API_H\nextern int api_errno;\nnamespace ns {\nint counter = 0;\nconst int limit = 4;\nnamespace inner { static char *names[2]; }\n}\nextern \"C\" {\nint c_count;\n}\nextern \"C\" int c_single;\nclass Box {\n  int m;\n};\nvoid f() { int local = 1; }\n#endif\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(
            &g,
            &[NodeKind::Variable, NodeKind::Const, NodeKind::Property]
        ),
        [
            (
                "api_errno",
                NodeKind::Variable,
                (2, 0, 2, 21),
                None,
                s("extern int")
            ),
            (
                "counter",
                NodeKind::Variable,
                (4, 0, 4, 16),
                s("ns"),
                s("int")
            ),
            (
                "limit",
                NodeKind::Const,
                (5, 0, 5, 20),
                s("ns"),
                s("const int")
            ),
            (
                "names",
                NodeKind::Variable,
                (6, 18, 6, 40),
                s("inner"),
                s("static char *")
            ),
            ("c_count", NodeKind::Variable, (9, 0, 9, 12), None, s("int")),
            (
                "c_single",
                NodeKind::Variable,
                (11, 11, 11, 24),
                None,
                s("int")
            ),
            ("m", NodeKind::Property, (13, 2, 13, 8), s("Box"), s("int")),
        ]
    );
}

/// Off-task 1 guard: a `.cpp` file's top-level variable keeps today's
/// record, and an anonymous namespace owns nothing.
#[test]
fn test_cpp_top_level_and_anonymous_namespace_variables_have_no_owner() {
    let g = parse(
        "state.cpp",
        b"int top = 1;\nnamespace {\nint hidden = 2;\n}\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Variable]),
        [
            ("top", NodeKind::Variable, (0, 0, 0, 12), None, s("int")),
            ("hidden", NodeKind::Variable, (2, 0, 2, 15), None, s("int")),
        ]
    );
}

// ---- Round 3 (P6 group A) ----

/// A-R1 (`jemalloc/internal/bit_util.h:53`, `nlohmann/detail/macro_scope.hpp:233`):
/// a comment in a multi-line `#define` ends its `preproc_arg`, so the rest
/// of the macro body parses as code. The locals of that body are no
/// Variables; the real globals before and after the macro stay.
#[test]
fn test_cpp_macro_body_locals_after_comment_emit_no_variable() {
    let pow2 = parse(
        "bit_util.h",
        b"static int kept = 1;\n#define POW2_CEIL(x) do {\t\\\n\tif (sizeof(x) > 4) {\t\\\n\t\t/*\t\t\\\n\t\t * keep the shift defined\t\\\n\t\t */\t\t\\\n\t\tint constant_32 = sizeof(x) * 4;\t\\\n\t\tx |= (x >> constant_32);\t\\\n\t}\t\t\\\n} while (0)\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&pow2, &[NodeKind::Variable, NodeKind::Const]),
        [(
            "kept",
            NodeKind::Variable,
            (0, 0, 0, 20),
            None,
            s("static int")
        )]
    );
    let serialize = parse(
        "macro_scope.hpp",
        b"static int kept = 1;\n#define SERIALIZE_ENUM(ENUM_TYPE, ...)                                   \\\n    template<typename BasicJsonType>                                     \\\n    inline void to_json(BasicJsonType& j, const ENUM_TYPE& e)            \\\n    {                                                                    \\\n        /* NOLINTNEXTLINE(modernize-avoid-c-arrays) */                   \\\n        static const std::pair<ENUM_TYPE, BasicJsonType> m[] = __VA_ARGS__; \\\n        auto it = std::find_if(std::begin(m), std::end(m), e);           \\\n        j = it->second;                                                  \\\n    }\nint after = 2;\n",
    );
    assert_eq!(
        rows_of_kinds(&serialize, &[NodeKind::Variable, NodeKind::Const]),
        [
            (
                "kept",
                NodeKind::Variable,
                (0, 0, 0, 20),
                None,
                s("static int")
            ),
            ("after", NodeKind::Variable, (10, 0, 10, 14), None, s("int")),
        ]
    );
}

/// A-R2 (`tests/abi/include/nlohmann/json_v3_10_5.hpp:12103`): a block the
/// grammar cannot place in a class body makes error recovery lift the
/// following members to namespace scope. `m_it` sits inside the class's
/// braces, so it is no namespace Variable. Variables in a namespace whose
/// `{` follows a comment, and in an `extern "C"` block, are file scope and
/// stay.
#[test]
fn test_cpp_members_lifted_by_error_recovery_emit_no_variable() {
    let g = parse(
        "iter_impl.hpp",
        b"namespace detail\n{\nclass iter_impl\n{\n    {\n        m_it = 0;\n    }\n    pointer m_object = nullptr;\n    internal_iterator<T> m_it {};\n};\n}  // namespace detail\nnamespace // anonymous\n{\nint kept_counter = 0;\n}\nextern \"C\" {\nint c_counter;\n}\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Variable, NodeKind::Const]),
        [
            (
                "kept_counter",
                NodeKind::Variable,
                (13, 0, 13, 21),
                None,
                s("int")
            ),
            (
                "c_counter",
                NodeKind::Variable,
                (16, 0, 16, 14),
                None,
                s("int")
            ),
        ]
    );
}

/// A-R3 (`jemalloc/internal/prof_recent.h:18`, `prof_structs.h:101`,
/// `xxhash.h:2655`): tree-sitter-cpp reads `typedef ql_head(T) list_t;` as
/// `typedef ql_head (T)` with a missing `;`. The alias is the name after
/// the macro call, never its argument. An attribute-macro typedef whose
/// alias the tree loses gets no Typedef at all (never `__aligned__`). A
/// trailing attribute macro after a real alias keeps the alias.
#[test]
fn test_cpp_macro_call_typedef_takes_alias_after_the_call() {
    let g = parse(
        "prof_structs.h",
        b"typedef ql_head(prof_recent_t) prof_recent_list_t;\ntypedef rb_tree(prof_tctx_t) prof_tctx_tree_t;\ntypedef __attribute__((__aligned__(1))) __attribute__((__may_alias__)) xxh_u32 xxh_unalign32;\ntypedef xxh_u64x2 xxh_aliasing_u64x2 XXH_ALIASING;\ntypedef void (redisCallbackFn)(struct redisAsyncContext*, void*);\n",
    );
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Typedef]),
        [
            (
                "prof_recent_list_t",
                NodeKind::Typedef,
                (0, 0, 0, 50),
                None,
                None
            ),
            (
                "prof_tctx_tree_t",
                NodeKind::Typedef,
                (1, 0, 1, 46),
                None,
                None
            ),
            (
                "xxh_aliasing_u64x2",
                NodeKind::Typedef,
                (3, 0, 3, 36),
                None,
                None
            ),
            (
                "redisCallbackFn",
                NodeKind::Typedef,
                (4, 0, 4, 65),
                None,
                None
            ),
        ]
    );
    assert_eq!(
        named(&g, "prof_recent_list_t").content_hash,
        xxh3_64_bytes(b"typedef ql_head(prof_recent_t) prof_recent_list_t;")
    );
}

/// A-R4 (`src/server.h:3461`, `src/redismodule.h:1276`): a prototype read
/// as a direct initialisation has type `void`, and a trailing attribute
/// macro read as one names a macro of the file: neither is a Variable.
/// Real direct-initialised globals and a `void *` global stay.
#[test]
fn test_cpp_void_direct_init_and_macro_call_emit_no_variable() {
    let g = parse(
        "server.h",
        b"#define ATTR_PRINTF(idx,cnt) __attribute__((format(printf,idx,cnt)))\nAPI void (*Module_Log)(Ctx *ctx, const char *fmt, ...) ATTR ATTR_PRINTF(3,4);\nvoid replDataBufReadFromConn(connection *conn, replDataBuf *buf, void (*error_handler)(connection *conn));\nstatic Foo g(1, 2);\nstd::string s(\"x\");\nvoid *cursor;\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Variable, NodeKind::Const]),
        [
            (
                "g",
                NodeKind::Variable,
                (3, 0, 3, 19),
                None,
                s("static Foo")
            ),
            (
                "s",
                NodeKind::Variable,
                (4, 0, 4, 19),
                None,
                s("std::string")
            ),
            (
                "cursor",
                NodeKind::Variable,
                (5, 0, 5, 13),
                None,
                s("void *")
            ),
        ]
    );
}

/// A-R6 (`nlohmann/detail/input/binary_reader.hpp:3045`,
/// `Fuzzer/FuzzerDictionary.h:71`): a method body sees its class's data
/// members, in the class body and in an out-of-class definition, and a
/// parameter of the same name hides the member. A free function sees no
/// member.
#[test]
fn test_cpp_member_receivers_type_calls_in_their_class_methods() {
    let g = parse(
        "binary_reader.hpp",
        b"struct json_sax_t { bool start_object(int n); };\nclass binary_reader {\n  public:\n    bool parse() { return sax->start_object(1); }\n    bool parse_other(other_t* sax) { return sax->start_object(2); }\n    bool later();\n    void print() { print_ascii(W.data(), W.size()); }\n  private:\n    json_sax_t* sax = nullptr;\n    Word W;\n};\nbool binary_reader::later() { return sax->start_object(3); }\nbool free_fn() { return sax->start_object(4); }\n",
    );
    let dynamic = CallMeta::FLAG_CALLBACK | CallMeta::FLAG_DYNAMIC_DISPATCH;
    assert_eq!(
        call_meta_rows(&g),
        [
            ("parse", (3, 4, 3, 49), dynamic, "json_sax_t"),
            ("parse_other", (4, 4, 4, 67), dynamic, "other_t"),
            ("print", (6, 4, 6, 53), dynamic, "Word"),
            ("print", (6, 4, 6, 53), dynamic, "Word"),
            ("later", (11, 0, 11, 60), dynamic, "json_sax_t"),
        ]
    );
}

/// A-R7 (`Fuzzer/FuzzerDriver.cpp:43`) and A-R8
/// (`Fuzzer/FuzzerUtilWindows.cpp:29`): an anonymous struct type is named by
/// its keyword, a named one by keyword and tag, never by its body. A
/// pointer to `const` is a mutable Variable; a `const` pointer, a
/// `constexpr` one and a `const` scalar are Consts.
#[test]
fn test_cpp_struct_body_types_and_pointer_constness() {
    let g = parse(
        "driver.hpp",
        b"struct {\n#define FLAG_INT(Name) int Name;\n#include \"Flags.def\"\n#undef FLAG_INT\n} Flags;\nstatic struct Named { int b; } named, *named_ptr;\nstatic const FuzzingOptions* HandlerOpt = nullptr;\nHandler *const fixed = nullptr;\nconstexpr const char* name = \"x\";\nconst int limit = 4;\nconst char* const both = \"y\";\n",
    );
    let s = Some;
    assert_eq!(
        rows_of_kinds(&g, &[NodeKind::Variable, NodeKind::Const]),
        [
            ("Flags", NodeKind::Variable, (0, 0, 4, 8), None, s("struct")),
            (
                "named",
                NodeKind::Variable,
                (5, 0, 5, 49),
                None,
                s("static struct Named")
            ),
            (
                "named_ptr",
                NodeKind::Variable,
                (5, 0, 5, 49),
                None,
                s("static struct Named *")
            ),
            (
                "HandlerOpt",
                NodeKind::Variable,
                (6, 0, 6, 50),
                None,
                s("static const FuzzingOptions*")
            ),
            (
                "fixed",
                NodeKind::Const,
                (7, 0, 7, 31),
                None,
                s("Handler *const")
            ),
            (
                "name",
                NodeKind::Const,
                (8, 0, 8, 33),
                None,
                s("constexpr const char*")
            ),
            (
                "limit",
                NodeKind::Const,
                (9, 0, 9, 20),
                None,
                s("const int")
            ),
            (
                "both",
                NodeKind::Const,
                (10, 0, 10, 29),
                None,
                s("const char* const")
            ),
        ]
    );
}

/// A-R5 follow-up (`tests/src/unit-to_chars.cpp`, `auto check_double =
/// [](...)`): a variable a lambda initialises is a call target, so `f(1)`
/// keeps its CALLBACK record with the declared type text. A direct call to
/// a prototyped function next to it gets no record.
#[test]
fn test_cpp_lambda_variable_call_keeps_callback_record() {
    let g = parse(
        "lambda.cpp",
        b"int g(int x);\nvoid run() {\n    auto f = [](int x) { return x; };\n    f(1);\n    g(2);\n}\n",
    );
    assert_eq!(
        call_meta_rows(&g),
        [("run", (1, 0, 5, 1), CallMeta::FLAG_CALLBACK, "auto")]
    );
}
