//! `CppProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each expectation is the record the hand-written loop produced
//! before the move (read from a `LocalGraph` dump of the old binary), in
//! emitted order.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    CppProvider::new()
        .expect("CppProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.cpp", src)
}

/// `(name, kind, span, owner_class)`
type Row<'g> = (&'g str, NodeKind, Span, Option<&'g str>);

fn rows(g: &LocalGraph) -> Vec<Row<'_>> {
    g.nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.owner_class.as_deref()))
        .collect()
}

fn named<'g>(g: &'g LocalGraph, name: &str, kind: NodeKind) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} {name}: {:?}", rows(g)))
}

#[test]
fn test_cpp_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.cpp"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// Sorted by `(imported_name, source, alias)` and deduped on all three: the
/// repeated include and the repeated `fs` alias collapse, the `as` alias of
/// the same namespace stays.
#[test]
fn test_cpp_driver_imports_only_file_sorts_and_dedupes_on_alias() {
    let g = parse(
        b"#include <vector>\n#include \"b.h\"\n#include <vector>\nnamespace fs = std;\nnamespace fs = std;\nnamespace as = std;\n",
    );
    assert!(g.nodes.is_empty(), "{:?}", rows(&g));
    let imports: Vec<(&str, &str, Option<&str>)> = g
        .imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        [
            ("b.h", "b.h", None),
            ("std", "std", Some("as")),
            ("std", "std", Some("fs")),
            ("vector", "vector", None),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
}

/// The tree parses `EXPORT_API` blanked, so `Foo` is a class; the hash of
/// the namespace that holds the macro still reads the original bytes.
#[test]
fn test_cpp_driver_decl_prefix_macro_hashes_original_bytes() {
    let src = "namespace n {\nEXPORT_API class Foo {\n  void run() { go(); }\n};\n}\n";
    let g = parse(src.as_bytes());
    assert_eq!(
        rows(&g),
        [
            ("n", NodeKind::Namespace, (0, 0, 4, 1), None),
            ("Foo", NodeKind::Class, (1, 11, 3, 1), Some("n")),
            ("run", NodeKind::Method, (2, 2, 2, 22), Some("Foo")),
        ]
    );
    let ns = &g.nodes[0];
    let body = &src[..src.len() - 1];
    assert_eq!(ns.content_hash, 5023803387696732205);
    assert_eq!(ns.content_hash, xxh3_64_bytes(body.as_bytes()));
    let patched = body.replace("EXPORT_API", "          ");
    assert_ne!(ns.content_hash, xxh3_64_bytes(patched.as_bytes()));
    assert_eq!(g.nodes[1].content_hash, 2595727911232537773);
    assert_eq!(g.nodes[2].calls, ["go"]);
}

/// Nested class and namespace owners, inline and out-of-line constructor
/// promotion, the three qualified-owner outcomes (class in file, namespace
/// in file, neither), a template qualifier and a friend restamped to the
/// namespace.
#[test]
fn test_cpp_driver_owners_file_keeps_kinds_and_owners() {
    let g = parse(
        b"namespace ns {\nclass Foo {\npublic:\n  Foo() {}\n  void inl() { helper(); }\n  class Inner {\n    void deep() {}\n  };\n  friend void pal(Foo&) {}\n};\nvoid free_fn() {}\n}\nFoo::Foo(int) {}\nvoid Foo::bar() { this->inl(); }\nvoid ns::later() {}\nvoid Other::reset() {}\ntemplate <typename T> void Box<T>::put() {}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("ns", NodeKind::Namespace, (0, 0, 11, 1), None),
            ("Foo", NodeKind::Class, (1, 0, 9, 1), Some("ns")),
            ("Foo", NodeKind::Constructor, (3, 2, 3, 10), Some("Foo")),
            ("inl", NodeKind::Method, (4, 2, 4, 26), Some("Foo")),
            ("Inner", NodeKind::Class, (5, 2, 7, 3), Some("Foo")),
            ("deep", NodeKind::Method, (6, 4, 6, 18), Some("Inner")),
            ("pal", NodeKind::Function, (8, 9, 8, 26), Some("ns")),
            ("free_fn", NodeKind::Function, (10, 0, 10, 17), Some("ns")),
            ("Foo", NodeKind::Constructor, (12, 0, 12, 16), Some("Foo")),
            ("bar", NodeKind::Method, (13, 0, 13, 32), Some("Foo")),
            ("later", NodeKind::Function, (14, 0, 14, 19), Some("ns")),
            ("reset", NodeKind::Method, (15, 0, 15, 22), Some("Other")),
            ("put", NodeKind::Method, (16, 22, 16, 43), Some("Box")),
        ]
    );
    assert_eq!(named(&g, "bar", NodeKind::Method).calls, ["Foo.inl"]);
    assert!(g.nodes.iter().all(|n| !n.is_exported));
}

/// The pre-scan marks the `override` definition only; its sentinel leads
/// the decorators.
#[test]
fn test_cpp_driver_override_method_gets_sentinel_first() {
    let g = parse(
        b"class Base {\npublic:\n  virtual void run() {}\n  virtual ~Base() {}\n};\nclass Derived : public Base {\npublic:\n  void run() override { Base::run(); }\n};\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("Base", NodeKind::Class, (0, 0, 4, 1), None),
            ("run", NodeKind::Method, (2, 2, 2, 23), Some("Base")),
            ("~Base", NodeKind::Method, (3, 2, 3, 20), Some("Base")),
            ("Derived", NodeKind::Class, (5, 0, 8, 1), None),
            ("run", NodeKind::Method, (7, 2, 7, 38), Some("Derived")),
        ]
    );
    assert!(g.nodes[1].decorators.is_empty());
    assert_eq!(g.nodes[4].decorators, [OVERRIDE_SENTINEL]);
    assert_eq!(g.nodes[4].calls, ["Base.run"]);
    assert_eq!(g.nodes[3].heritage, ["Base"]);
}

/// A lambda argument with a call becomes an `<anonymous>` node after its
/// enclosing function; one without a call emits nothing.
#[test]
fn test_cpp_driver_lambda_with_call_emits_one_anonymous_node() {
    let g = parse(
        b"#include <algorithm>\nvoid f(std::vector<int>& v) {\n  std::for_each(v.begin(), v.end(), [](int x) { use(x); });\n  std::sort(v.begin(), v.end(), [](int a, int b) { return a < b; });\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("f", NodeKind::Function, (1, 0, 4, 1), None),
            ("<anonymous:3:36>", NodeKind::Function, (2, 36, 2, 57), None),
        ]
    );
    let anon = &g.nodes[1];
    assert_eq!(anon.calls, ["use"]);
    assert_eq!(anon.content_hash, 7572169262107522346);
    assert_eq!(anon.content_hash, xxh3_64_bytes(b"[](int x) { use(x); }"));
}

/// A `#define` the tree lost comes back after the calls: exported, with no
/// content hash, after every tree node.
#[test]
fn test_cpp_driver_lost_define_pushes_fallback_macro_last() {
    let g = parse(b"int f(int a, int b\n#define MID 1\n) { return a; }\n");
    assert_eq!(
        rows(&g),
        [
            ("f", NodeKind::Function, (0, 0, 2, 15), None),
            ("MID", NodeKind::Macro, (1, 8, 1, 11), None),
        ]
    );
    let mid = &g.nodes[1];
    assert!(mid.is_exported);
    assert_eq!(mid.content_hash, 0);
}

/// A mixed-case macro line hides the class from the tree; the lexical
/// recovery appends the class and owns its method.
#[test]
fn test_cpp_driver_lost_class_recovered_lexically_after_members() {
    let g = parse(b"MyApi\nclass Lost {\n  void m() { c(); }\n};\n");
    assert_eq!(
        rows(&g),
        [
            ("m", NodeKind::Method, (2, 2, 2, 19), Some("Lost")),
            ("Lost", NodeKind::Class, (1, 0, 3, 1), None),
        ]
    );
    assert_eq!(g.nodes[0].calls, ["c"]);
    let lost = &g.nodes[1];
    assert!(!lost.is_exported);
    assert_eq!(lost.content_hash, 4538484169412695894);
}

/// A header exports every declaration; the same source as `.cpp` exports
/// none.
#[test]
fn test_cpp_driver_header_file_exports_every_node() {
    let src = b"class Api {\npublic:\n  int x;\n  void m();\n};\nint g;\nconst int K = 1;\n";
    let want = [
        ("Api", NodeKind::Class, (0, 0, 4, 1), None),
        ("x", NodeKind::Property, (2, 2, 2, 8), Some("Api")),
        ("m", NodeKind::Method, (3, 2, 3, 11), Some("Api")),
        ("g", NodeKind::Variable, (5, 0, 5, 6), None),
        ("K", NodeKind::Const, (6, 0, 6, 16), None),
    ];
    let h = parse_at("api.h", src);
    assert_eq!(rows(&h), want);
    assert!(h.nodes.iter().all(|n| n.is_exported));
    let types: Vec<Option<&str>> = h
        .nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect();
    assert_eq!(
        types,
        [
            None,
            Some("int"),
            Some("void"),
            Some("int"),
            Some("const int")
        ]
    );
    let cpp = parse_at("api.cpp", src);
    assert_eq!(rows(&cpp), want);
    assert!(cpp.nodes.iter().all(|n| !n.is_exported));
}

/// The parser's one blind spot, `cpp-dlsym`, spans the call and takes
/// `is_test` from the path category.
#[test]
fn test_cpp_driver_dlsym_call_emits_blind_spot() {
    let src = b"void load(void* h) {\n  void* p = dlsym(h, \"sym\");\n}\n";
    for (path, is_test) in [("load.cpp", false), ("tests/load_test.cpp", true)] {
        let g = parse_at(path, src);
        let spots: Vec<(&str, Span, bool)> = g
            .blind_spots
            .iter()
            .map(|b| (b.kind.as_str(), b.span, b.is_test))
            .collect();
        assert_eq!(spots, [("cpp-dlsym", (1, 12, 1, 27), is_test)], "{path}");
        assert_eq!(g.blind_spots[0].file_path, Path::new(path));
        assert_eq!(rows(&g), [("load", NodeKind::Function, (0, 0, 2, 1), None)]);
    }
}
