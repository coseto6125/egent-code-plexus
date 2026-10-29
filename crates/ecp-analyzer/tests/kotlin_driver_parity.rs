//! `KotlinProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move; node kinds, owners and spans were cross-checked against
//! a graph the pre-move parser built from the same sources.

use ecp_analyzer::framework_confidence::KTOR_ROUTE;
use ecp_analyzer::framework_helpers::MODULE_LEVEL_SOURCE;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

fn parse(src: &[u8]) -> LocalGraph {
    KotlinProvider::new()
        .expect("KotlinProvider::new")
        .parse_file(Path::new("a.kt"), src)
        .expect("parse_file")
}

fn names_kinds(g: &LocalGraph) -> Vec<(&str, NodeKind)> {
    g.nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

/// The only node with this name and kind.
fn node<'g>(g: &'g LocalGraph, name: &str, kind: NodeKind) -> &'g RawNode {
    let hits: Vec<&RawNode> = g
        .nodes
        .iter()
        .filter(|n| n.name == name && n.kind == kind)
        .collect();
    assert_eq!(hits.len(), 1, "{name} {kind:?}: {:?}", names_kinds(g));
    hits[0]
}

/// `(span, is_exported, owner_class)`
type Shape<'g> = ((u32, u32, u32, u32), bool, Option<&'g str>);

fn shape(n: &RawNode) -> Shape<'_> {
    (n.span, n.is_exported, n.owner_class.as_deref())
}

#[test]
fn test_kotlin_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.kt"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// Imports keep source order with no sort; a wildcard names `*` and keeps
/// the package as its source.
#[test]
fn test_kotlin_driver_imports_only_file_keeps_source_order() {
    let g = parse(b"import a.b.C\nimport x.y.*\nimport p.Q as R\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
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
            ("a.b.C", "C", None),
            ("x.y", "*", None),
            ("p.Q", "Q", Some("R"))
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
}

/// The primary constructor is its own node, rooted at the parameter list.
/// `internal` hides the class only: the constructor match has no `@export`.
#[test]
fn test_kotlin_driver_primary_constructor_and_data_class_emit_class_and_constructor() {
    let src: &[u8] = b"class Foo(val x: Int)\ninternal data class Point(val x: Int, val y: Int)\n";
    let g = parse(src);
    let foo = node(&g, "Foo", NodeKind::Class);
    assert_eq!(shape(foo), ((0, 0, 0, 21), true, None));
    assert_eq!(foo.content_hash, xxh3_64_bytes(b"class Foo(val x: Int)"));
    let foo_ctor = node(&g, "Foo", NodeKind::Constructor);
    assert_eq!(shape(foo_ctor), ((0, 9, 0, 21), true, Some("Foo")));
    assert_eq!(foo_ctor.content_hash, xxh3_64_bytes(b"(val x: Int)"));
    let point = node(&g, "Point", NodeKind::Class);
    assert_eq!(shape(point), ((1, 0, 1, 49), false, None));
    let point_ctor = node(&g, "Point", NodeKind::Constructor);
    assert_eq!(shape(point_ctor), ((1, 25, 1, 49), true, Some("Point")));
    assert_eq!(g.nodes.len(), 4, "{:?}", names_kinds(&g));
}

#[test]
fn test_kotlin_driver_object_declaration_member_is_method() {
    let g = parse(b"object Registry {\n    fun register() {}\n}\n");
    let registry = node(&g, "Registry", NodeKind::Class);
    assert_eq!(shape(registry), ((0, 0, 2, 1), true, None));
    let register = node(&g, "register", NodeKind::Method);
    assert_eq!(shape(register), ((1, 4, 1, 21), true, Some("Registry")));
    assert_eq!(register.content_hash, xxh3_64_bytes(b"fun register() {}"));
}

/// The receiver `user_type` precedes the name, so `@type` stays empty.
#[test]
fn test_kotlin_driver_extension_function_is_top_level_function() {
    let src: &[u8] = b"fun String.shout() { println(this) }\n";
    let g = parse(src);
    let shout = node(&g, "shout", NodeKind::Function);
    assert_eq!(shape(shout), ((0, 0, 0, 36), true, None));
    assert_eq!(shout.type_annotation, None);
    assert!(shout.decorators.is_empty() && shout.heritage.is_empty());
    assert_eq!(shout.content_hash, xxh3_64_bytes(&src[..36]));
}

#[test]
fn test_kotlin_driver_nested_class_owner_is_tightest_class() {
    let g = parse(b"class Outer {\n    class Inner {\n        fun run() {}\n    }\n}\n");
    assert_eq!(
        shape(node(&g, "Outer", NodeKind::Class)),
        ((0, 0, 4, 1), true, None)
    );
    assert_eq!(
        shape(node(&g, "Inner", NodeKind::Class)),
        ((1, 4, 3, 5), true, Some("Outer"))
    );
    assert_eq!(
        shape(node(&g, "run", NodeKind::Method)),
        ((2, 8, 2, 20), true, Some("Inner"))
    );
}

/// Kotlin accepts a top-level `Function` as a tx scope, unlike Java.
#[test]
fn test_kotlin_driver_transactional_method_and_function_emit_tx_scopes() {
    let g = parse(
        b"import org.springframework.transaction.annotation.Transactional\n\nclass OrderService {\n    @Transactional\n    fun placeOrder() {}\n\n    fun listOrders() {}\n}\n\n@Transactional\nfun topLevel() {}\n",
    );
    let place = node(&g, "placeOrder", NodeKind::Method);
    assert_eq!(shape(place), ((3, 4, 4, 23), true, Some("OrderService")));
    assert_eq!(place.decorators, ["@Transactional"]);
    let top = node(&g, "topLevel", NodeKind::Function);
    assert_eq!(shape(top), ((9, 0, 10, 17), true, None));
    assert_eq!(top.decorators, ["@Transactional"]);
    assert!(node(&g, "listOrders", NodeKind::Method)
        .decorators
        .is_empty());
    let scopes: Vec<(&str, FrameworkId)> = g
        .tx_scopes
        .as_deref()
        .expect("tx scopes")
        .iter()
        .map(|s| (g.nodes[s.node_idx() as usize].name.as_str(), s.framework()))
        .collect();
    assert_eq!(
        scopes,
        [
            ("placeOrder", FrameworkId::SpringTransactional),
            ("topLevel", FrameworkId::SpringTransactional)
        ]
    );
}

const KTOR_SRC: &[u8] = b"import io.ktor.server.routing.*\n\nfun Application.module() {\n    routing {\n        get(\"/x\") { call.respondText(\"ok\") }\n    }\n}\n";

/// The ref spans the whole `get(...) { }` call; both lambdas hold a call.
#[test]
fn test_kotlin_driver_ktor_routing_block_emits_module_level_ref() {
    let g = parse(KTOR_SRC);
    let refs: Vec<_> = g
        .framework_refs
        .iter()
        .map(|r| {
            (
                r.source_name.as_str(),
                r.target_name.as_str(),
                r.reason.as_str(),
                r.confidence,
                r.span,
            )
        })
        .collect();
    assert_eq!(
        refs,
        [(
            MODULE_LEVEL_SOURCE,
            "/x",
            "ktor-route-get",
            KTOR_ROUTE,
            (4, 8, 4, 44)
        )]
    );
    node(&g, "module", NodeKind::Function);
    node(&g, "<anonymous:4:12>", NodeKind::Function);
    node(&g, "<anonymous:5:18>", NodeKind::Function);
}

#[test]
fn test_kotlin_driver_ktor_without_import_emits_no_ref() {
    let src = std::str::from_utf8(KTOR_SRC).expect("utf8");
    let g = parse(
        src.replacen("import io.ktor.server.routing.*", "", 1)
            .as_bytes(),
    );
    assert!(g.framework_refs.is_empty(), "{:?}", g.framework_refs);
}

/// A trailing and a paren-position lambda with a call each get one node; a
/// lambda with no call gets none.
#[test]
fn test_kotlin_driver_lambda_argument_with_call_emits_anonymous_node() {
    let src: &[u8] = b"fun f() {\n    list.forEach { process(it) }\n    list.map({ x -> g(x) })\n    list.map { it + 1 }\n}\n";
    let g = parse(src);
    let anon: Vec<(&str, (u32, u32, u32, u32), u64)> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("<anonymous"))
        .map(|n| (n.name.as_str(), n.span, n.content_hash))
        .collect();
    assert_eq!(
        anon,
        [
            (
                "<anonymous:2:17>",
                (1, 17, 1, 32),
                xxh3_64_bytes(b"{ process(it) }")
            ),
            (
                "<anonymous:3:13>",
                (2, 13, 2, 26),
                xxh3_64_bytes(b"{ x -> g(x) }")
            ),
        ]
    );
    let lambda = node(&g, "<anonymous:2:17>", NodeKind::Function);
    assert!(!lambda.is_exported);
}

/// `class A { fun f() {} }` parses to an ERROR node until the post-tree
/// recovery re-parses it; spans and hashes still read the original bytes.
#[test]
fn test_kotlin_driver_same_line_closer_recovers_class_and_method() {
    let src: &[u8] = b"class A { fun f() {} }\n";
    let g = parse(src);
    let a = node(&g, "A", NodeKind::Class);
    assert_eq!(shape(a), ((0, 0, 0, 22), true, None));
    assert_eq!(a.content_hash, xxh3_64_bytes(b"class A { fun f() {} }"));
    let f = node(&g, "f", NodeKind::Method);
    assert_eq!(shape(f), ((0, 10, 0, 20), true, Some("A")));
}

/// The error-free misparse: a top-level `infix_expression` led by an
/// `object_literal`.
#[test]
fn test_kotlin_driver_same_line_object_closer_recovers_object() {
    let g = parse(b"object O { fun a() {} }\n");
    assert_eq!(
        shape(node(&g, "O", NodeKind::Class)),
        ((0, 0, 0, 23), true, None)
    );
    assert_eq!(
        shape(node(&g, "a", NodeKind::Method)),
        ((0, 11, 0, 21), true, Some("O"))
    );
}

/// The general and the `override` function patterns share the root, so
/// they merge into one node that carries the sentinel.
#[test]
fn test_kotlin_driver_override_merges_into_one_method_with_sentinel() {
    let g = parse(b"class B : A() {\n    override fun f() {}\n}\n");
    let b = node(&g, "B", NodeKind::Class);
    assert_eq!(b.heritage, ["A"]);
    let f = node(&g, "f", NodeKind::Method);
    assert_eq!(shape(f), ((1, 4, 1, 23), true, Some("B")));
    assert_eq!(f.decorators, [OVERRIDE_SENTINEL]);
}

#[test]
fn test_kotlin_driver_reflection_calls_emit_blind_spots_in_order() {
    let g = parse(b"fun f() {\n    val c = Class.forName(\"x\")\n    m.invoke(o)\n}\n");
    let spots: Vec<(&str, (u32, u32, u32, u32), bool)> = g
        .blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span, b.is_test))
        .collect();
    assert_eq!(
        spots,
        [
            ("kt-class-forname", (1, 12, 1, 30), false),
            ("kt-method-invoke", (2, 4, 2, 15), false)
        ]
    );
    assert!(g
        .blind_spots
        .iter()
        .all(|b| b.file_path == Path::new("a.kt")));
}
