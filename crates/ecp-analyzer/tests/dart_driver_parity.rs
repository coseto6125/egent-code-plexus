//! `DartProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move (read from a dump of the old binary), in their emitted
//! order where the order is fixed by the step list.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode, RawTxScope};
use ecp_core::graph::NodeKind;
use std::path::Path;

type Span4 = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    DartProvider::new()
        .expect("DartProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.dart", src)
}

fn names_kinds(g: &LocalGraph) -> Vec<(&str, NodeKind)> {
    g.nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

/// `(name, kind, span, owner_class)` of every node, in emitted order.
type NodeRow<'g> = (&'g str, NodeKind, Span4, Option<&'g str>);

fn rows(g: &LocalGraph) -> Vec<NodeRow<'_>> {
    g.nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.owner_class.as_deref()))
        .collect()
}

fn types(g: &LocalGraph) -> Vec<Option<&str>> {
    g.nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect()
}

fn at(g: &LocalGraph, i: usize) -> &RawNode {
    g.nodes
        .get(i)
        .unwrap_or_else(|| panic!("no node {i} in {:?}", names_kinds(g)))
}

/// `(source, imported_name, alias)`
type ImportRow<'g> = (&'g str, &'g str, Option<&'g str>);

fn imports(g: &LocalGraph) -> Vec<ImportRow<'_>> {
    g.imports
        .iter()
        .map(|i| {
            (
                i.source.as_str(),
                i.imported_name.as_str(),
                i.alias.as_deref(),
            )
        })
        .collect()
}

fn blind(g: &LocalGraph) -> Vec<(&str, Span4, bool)> {
    g.blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span, b.is_test))
        .collect()
}

#[test]
fn test_dart_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.dart"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// Imports keep source order with no sort and no dedupe; a `show`
/// combinator empties the imported name; the calls walk records the
/// package URIs as path literals in its stack order.
#[test]
fn test_dart_driver_imports_only_file_keeps_source_order() {
    let src = concat!(
        "import 'package:a/a.dart';\n",
        "import 'dart:async' as async;\n",
        "import 'package:b/b.dart' show B;\n",
        "import 'package:a/a.dart';\n",
    );
    let g = parse(src.as_bytes());
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    assert_eq!(
        imports(&g),
        [
            ("package:a/a.dart", "package:a/a.dart", None),
            ("dart:async", "dart:async", Some("async")),
            ("package:b/b.dart", "", None),
            ("package:a/a.dart", "package:a/a.dart", None),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    let literals: Vec<(&str, Span4)> = g
        .path_literals
        .as_deref()
        .expect("package URIs are path literals")
        .iter()
        .map(|p| (p.value.as_str(), p.span))
        .collect();
    assert_eq!(
        literals,
        [
            ("package:a/a.dart", (3, 7, 3, 25)),
            ("package:b/b.dart", (2, 7, 2, 25)),
            ("package:a/a.dart", (0, 7, 0, 25)),
        ]
    );
    assert!(g.blind_spots.is_empty() && g.tx_scopes.is_none());
}

/// Class, mixin and extension; a field, a getter, the default, named and
/// factory constructors and a method. A named constructor emits one node
/// per name identifier on the same span; the mixin is spliced into the
/// class heritage after the superclass; the stamp sets the owner.
#[test]
fn test_dart_driver_class_mixin_extension_emits_members_in_query_order() {
    let src = concat!(
        "class Animal {}\n",
        "mixin Walker on Animal {}\n",
        "class Dog extends Animal with Walker implements Comparable<Dog> {\n",
        "  final String name;\n",
        "  Dog(this.name);\n",
        "  Dog.named(this.name);\n",
        "  factory Dog.create() => Dog('x');\n",
        "  String get label => name;\n",
        "  void bark() {\n",
        "    print(name);\n",
        "  }\n",
        "}\n",
        "extension DogX on Dog {\n",
        "  void wag() => bark();\n",
        "}\n",
    );
    let g = parse(src.as_bytes());
    let dog = Some("Dog");
    assert_eq!(
        rows(&g),
        [
            ("Animal", NodeKind::Class, (0, 0, 0, 15), None),
            ("Walker", NodeKind::Trait, (1, 0, 1, 25), None),
            ("Dog", NodeKind::Class, (2, 0, 11, 1), None),
            ("name", NodeKind::Property, (3, 2, 3, 19), dog),
            ("Dog", NodeKind::Constructor, (4, 2, 4, 16), dog),
            ("Dog", NodeKind::Constructor, (5, 2, 5, 22), dog),
            ("named", NodeKind::Constructor, (5, 2, 5, 22), dog),
            ("Dog", NodeKind::Constructor, (6, 2, 6, 35), dog),
            ("create", NodeKind::Constructor, (6, 2, 6, 35), dog),
            ("label", NodeKind::Property, (7, 2, 7, 27), dog),
            ("bark", NodeKind::Method, (8, 2, 10, 3), dog),
            ("DogX", NodeKind::Trait, (12, 0, 14, 1), None),
            ("wag", NodeKind::Method, (13, 2, 13, 23), Some("DogX")),
        ]
    );
    assert_eq!(
        types(&g),
        [
            None,
            None,
            None,
            Some("String"),
            None,
            None,
            None,
            None,
            None,
            Some("String"),
            Some("void"),
            None,
            Some("void"),
        ]
    );
    let class = at(&g, 2);
    assert_eq!(class.heritage, ["Animal", "Walker", "Comparable"]);
    assert_eq!(class.content_hash, 2203494413119208774);
    assert!(g
        .nodes
        .iter()
        .all(|n| n.is_exported && n.decorators.is_empty()));
    let bark = at(&g, 10);
    assert_eq!(bark.calls, ["print"]);
    assert_eq!(bark.field_reads, ["\u{1}name name"]);
    assert_eq!(bark.content_hash, 8394018138179969286);
    assert_eq!(at(&g, 7).calls, ["Dog"]);
    assert_eq!(at(&g, 12).calls, ["bark"]);
    let metas: Vec<Span4> = g.raw_function_metas.iter().map(|m| m.span).collect();
    assert_eq!(metas, [(6, 2, 6, 35), (8, 2, 10, 3), (13, 2, 13, 23)]);
    assert!(g.imports.is_empty() && g.tx_scopes.is_none() && g.path_literals.is_none());
}

/// An enum and its constants, a private and a generic top-level variable,
/// a new-style typedef the grammar misparses as a variable, and a
/// top-level function: the inner `function_signature` match is skipped,
/// and the field reads precede the variant refs.
#[test]
fn test_dart_driver_enum_variables_typedef_function_emit_once_each() {
    let src = concat!(
        "enum Color { red, green }\n",
        "int _hidden = 1;\n",
        "List<int> items = [];\n",
        "typedef Cb = void Function(int);\n",
        "void main() {\n",
        "  run(Color.red);\n",
        "}\n",
    );
    let g = parse(src.as_bytes());
    let color = Some("Color");
    assert_eq!(
        rows(&g),
        [
            ("Color", NodeKind::Enum, (0, 0, 0, 25), None),
            ("red", NodeKind::EnumVariant, (0, 13, 0, 16), color),
            ("green", NodeKind::EnumVariant, (0, 18, 0, 23), color),
            ("_hidden", NodeKind::Variable, (1, 0, 1, 16), None),
            ("items", NodeKind::Variable, (2, 0, 2, 21), None),
            ("Cb", NodeKind::Typedef, (3, 0, 3, 32), None),
            ("main", NodeKind::Function, (4, 0, 6, 1), None),
        ]
    );
    assert_eq!(
        types(&g),
        [
            None,
            None,
            None,
            Some("int"),
            Some("List"),
            None,
            Some("void")
        ]
    );
    let exported: Vec<bool> = g.nodes.iter().map(|n| n.is_exported).collect();
    assert_eq!(exported, [true, true, true, false, true, true, true]);
    assert_eq!(at(&g, 5).content_hash, 12136448907424914801);
    let main = at(&g, 6);
    assert_eq!(main.calls, ["run"]);
    assert_eq!(
        main.field_reads,
        ["red", "\u{1}name Color", "\u{1}variant Color.red"]
    );
    assert_eq!(main.content_hash, 16262225353354114572);
}

/// The optional `(type (_))` child and a qualified annotation name can
/// fire more than one match for one declaration. After the consecutive
/// dedupe on (name, span, kind), each declarator and the annotation
/// appear once, typed by the type name.
#[test]
fn test_dart_driver_duplicate_captures_dedupe_to_first() {
    let src = concat!(
        "List<int> a = [], b = [];\n",
        "class K {\n",
        "  Map<String, int> p = {}, q = {};\n",
        "}\n",
    );
    let g = parse(src.as_bytes());
    let k = Some("K");
    assert_eq!(
        rows(&g),
        [
            ("a", NodeKind::Variable, (0, 0, 0, 25), None),
            ("b", NodeKind::Variable, (0, 0, 0, 25), None),
            ("K", NodeKind::Class, (1, 0, 3, 1), None),
            ("p", NodeKind::Property, (2, 2, 2, 33), k),
            ("q", NodeKind::Property, (2, 2, 2, 33), k),
        ]
    );
    assert_eq!(
        types(&g),
        [Some("List"), Some("List"), None, Some("Map"), Some("Map")]
    );
    assert_eq!(at(&g, 0).content_hash, at(&g, 1).content_hash);

    let g = parse(b"List<int> only = [];\n");
    assert_eq!(
        rows(&g),
        [("only", NodeKind::Variable, (0, 0, 0, 20), None)]
    );
    assert_eq!(types(&g), [Some("List")]);

    let g = parse(b"@a.b.c\nvoid f() {}\n");
    assert_eq!(
        rows(&g),
        [
            ("c", NodeKind::Annotation, (0, 0, 0, 6), None),
            ("f", NodeKind::Function, (0, 0, 1, 11), None),
        ]
    );
}

/// `.transaction(closure)` binds the enclosing function once, even for two
/// calls; the closure with a call becomes an anonymous node, the empty
/// one does not.
#[test]
fn test_dart_driver_transaction_calls_bind_enclosing_function_once() {
    let src = concat!(
        "Future<void> save(Database db) async {\n",
        "  await db.transaction((txn) async {\n",
        "    await txn.insert('t', {});\n",
        "  });\n",
        "  await db.transaction((txn) async {});\n",
        "}\n",
    );
    let g = parse(src.as_bytes());
    assert_eq!(
        rows(&g),
        [
            ("save", NodeKind::Function, (0, 0, 5, 1), None),
            ("<anonymous:2:23>", NodeKind::Function, (1, 23, 3, 3), None),
        ]
    );
    assert_eq!(types(&g), [Some("Future<void>"), None]);
    assert_eq!(
        at(&g, 0).calls,
        ["Database.transaction", "Database.transaction"]
    );
    assert_eq!(at(&g, 1).calls, ["\u{1}from txn.insert"]);
    assert!(!at(&g, 1).is_exported);
    assert_eq!(
        g.tx_scopes.as_deref(),
        Some(&[RawTxScope::new(0, FrameworkId::DartTransaction)][..])
    );
}

/// A closure argument whose body calls emits one `<anonymous:row+1:col>`
/// Function node with the closure's span and bytes; `(x) => x` emits none.
#[test]
fn test_dart_driver_closure_with_call_emits_anonymous_node() {
    let src = concat!(
        "void walk(List<int> xs) {\n",
        "  xs.forEach((x) {\n",
        "    print(x);\n",
        "  });\n",
        "  xs.map((x) => x);\n",
        "}\n",
    );
    let g = parse(src.as_bytes());
    assert_eq!(
        rows(&g),
        [
            ("walk", NodeKind::Function, (0, 0, 5, 1), None),
            ("<anonymous:2:13>", NodeKind::Function, (1, 13, 3, 3), None),
        ]
    );
    assert_eq!(at(&g, 0).calls, ["List.map", "List.forEach"]);
    let anon = at(&g, 1);
    assert_eq!(anon.calls, ["print"]);
    assert!(!anon.is_exported && anon.type_annotation.is_none());
    assert_eq!(anon.content_hash, 17850257453211030801);
    assert!(g.tx_scopes.is_none());
}

/// Both `BLIND` rows push inside the query in match order, flagged by the
/// path category; the mirrors import is also an import record.
#[test]
fn test_dart_driver_blind_spots_emit_in_match_order_with_test_flag() {
    let src = concat!(
        "import 'dart:mirrors';\n",
        "void call(Function f) {\n",
        "  Function.apply(f, []);\n",
        "}\n",
    );
    let g = parse_at("test/h_blind_test.dart", src.as_bytes());
    assert_eq!(
        blind(&g),
        [
            ("dart-mirrors-import", (0, 0, 0, 22), true),
            ("dart-function-apply", (2, 2, 2, 23), true),
        ]
    );
    assert!(g.blind_spots[0]
        .hint
        .starts_with("import 'dart:mirrors' — "));
    assert!(g.blind_spots[1]
        .hint
        .starts_with("Function.apply(<fn>, <args>) — "));
    assert!(g
        .blind_spots
        .iter()
        .all(|b| b.file_path == Path::new("test/h_blind_test.dart")));
    assert_eq!(imports(&g), [("dart:mirrors", "dart:mirrors", None)]);
    assert_eq!(rows(&g), [("call", NodeKind::Function, (1, 0, 3, 1), None)]);
    assert_eq!(at(&g, 0).content_hash, 9245701781066280560);

    let g = parse_at(
        "lib/l_apply.dart",
        b"void call(Function f) {\n  Function.apply(f, []);\n}\n",
    );
    assert_eq!(blind(&g), [("dart-function-apply", (1, 2, 1, 23), false)]);
}
