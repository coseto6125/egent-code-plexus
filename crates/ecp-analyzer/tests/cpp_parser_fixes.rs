//! `CppProvider` defects found in the driver-move review
//! (FU-2026-10-30-955135c9e46b). Each test asserts the correct records, so
//! it fails until the fix lands.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
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
