//! `JavaProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order: query match order, which is
//! source order except where a pattern's captures start inside its root.

use ecp_analyzer::framework_confidence::{SPRING_AUTOWIRED, SPRING_ROUTE};
use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span4 = (u32, u32, u32, u32);

fn parse(src: &[u8]) -> LocalGraph {
    JavaProvider::new()
        .expect("JavaProvider::new")
        .parse_file(Path::new("A.java"), src)
        .expect("parse_file")
}

/// `(name, kind, span, owner_class)` of each node, in emitted order.
type Row<'g> = (&'g str, NodeKind, Span4, Option<&'g str>);

fn rows(g: &LocalGraph) -> Vec<Row<'_>> {
    g.nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.owner_class.as_deref()))
        .collect()
}

fn node<'g>(g: &'g LocalGraph, name: &str, kind: NodeKind) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} `{name}` in {:?}", rows(g)))
}

/// `(source_name, target_name, reason, confidence, span)`
type Ref<'g> = (&'g str, &'g str, &'g str, f32, Span4);

fn refs(g: &LocalGraph) -> Vec<Ref<'_>> {
    g.framework_refs
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
        .collect()
}

/// `(callable name, framework)` of each tx scope, in emitted order.
fn tx(g: &LocalGraph) -> Vec<(&str, FrameworkId)> {
    g.tx_scopes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| (g.nodes[s.node_idx() as usize].name.as_str(), s.framework()))
        .collect()
}

#[test]
fn test_java_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("A.java"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// A static import aliases its last segment; a wildcard aliases `*` or
/// `static:*`. The repeated `java.util.List` matches the `exists` scan and
/// is dropped; the rest keep source order.
#[test]
fn test_java_driver_imports_only_file_keeps_static_and_wildcard_aliases() {
    let g = parse(
        b"import java.util.List;\nimport static java.util.Collections.sort;\nimport java.io.*;\nimport static org.junit.Assert.*;\nimport java.util.List;\n",
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
            ("java.util.List", "List", None),
            ("java.util.Collections.sort", "sort", Some("sort")),
            ("java.io", "*", Some("*")),
            ("org.junit.Assert", "*", Some("static:*")),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.event_topics.is_none() && g.framework_refs.is_empty());
}

/// `int a, b;` keys its dedupe on each name, so two Property nodes share the
/// declaration's span and hash; the constructor and the method key on their
/// roots. Owner stamping names the class for every member.
#[test]
fn test_java_driver_class_with_constructor_and_method_emits_members_in_order() {
    let g = parse(
        b"public class Dog {\n    int a, b;\n    public Dog() {}\n    void bark() { sound(); }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("Dog", NodeKind::Class, (0, 0, 4, 1), None),
            ("a", NodeKind::Property, (1, 4, 1, 13), Some("Dog")),
            ("b", NodeKind::Property, (1, 4, 1, 13), Some("Dog")),
            ("Dog", NodeKind::Constructor, (2, 4, 2, 19), Some("Dog")),
            ("bark", NodeKind::Method, (3, 4, 3, 28), Some("Dog")),
        ]
    );
    let exported: Vec<bool> = g.nodes.iter().map(|n| n.is_exported).collect();
    assert_eq!(exported, [true, false, false, true, false]);
    let types: Vec<Option<&str>> = g
        .nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect();
    assert_eq!(types, [None, Some("int"), Some("int"), None, Some("void")]);
    assert_eq!(g.nodes[1].content_hash, xxh3_64_bytes(b"int a, b;"));
    assert_eq!(g.nodes[2].content_hash, xxh3_64_bytes(b"int a, b;"));
    assert_eq!(
        g.nodes[4].content_hash,
        xxh3_64_bytes(b"void bark() { sound(); }")
    );
    assert!(g
        .nodes
        .iter()
        .all(|n| n.decorators.is_empty() && n.heritage.is_empty()));
    assert!(g.tx_scopes.is_none());
}

/// `default` is no `public`/`protected` modifier, so the method is not exported.
#[test]
fn test_java_driver_interface_default_method_emits_owned_method() {
    let g = parse(b"public interface Shape {\n    default double area() { return 0; }\n}\n");
    assert_eq!(
        rows(&g),
        [
            ("Shape", NodeKind::Interface, (0, 0, 2, 1), None),
            ("area", NodeKind::Method, (1, 4, 1, 39), Some("Shape")),
        ]
    );
    let area = node(&g, "area", NodeKind::Method);
    assert_eq!(
        (area.type_annotation.as_deref(), area.is_exported),
        (Some("double"), false)
    );
    assert!(node(&g, "Shape", NodeKind::Interface).is_exported);
}

/// Annotation elements are Methods owned by the annotation type; the
/// `@interface` pattern has no `@export` capture.
#[test]
fn test_java_driver_annotation_type_members_emit_owned_methods() {
    let g = parse(
        b"public @interface Tag {\n    String value() default \"\";\n    int priority();\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("Tag", NodeKind::Annotation, (0, 0, 3, 1), None),
            ("value", NodeKind::Method, (1, 4, 1, 30), Some("Tag")),
            ("priority", NodeKind::Method, (2, 4, 2, 19), Some("Tag")),
        ]
    );
    let types: Vec<Option<&str>> = g
        .nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect();
    assert_eq!(types, [None, Some("String"), Some("int")]);
    assert!(g.nodes.iter().all(|n| !n.is_exported));
}

const SPRING_CONTROLLER: &str = "@RestController\npublic class UserController {\n    @Autowired\n    private UserRepo repo;\n    @GetMapping(\"/users\")\n    public String list() { return \"\"; }\n}\n";

/// The decorator match and the export match of one declaration merge on the
/// root; the Spring refs keep match order: autowired, then route.
#[test]
fn test_java_driver_spring_rest_controller_with_import_emits_refs() {
    let src = format!(
        "import org.springframework.web.bind.annotation.GetMapping;\n\n{SPRING_CONTROLLER}"
    );
    let g = parse(src.as_bytes());
    assert_eq!(
        rows(&g),
        [
            ("UserController", NodeKind::Class, (2, 0, 8, 1), None),
            (
                "repo",
                NodeKind::Property,
                (4, 4, 5, 26),
                Some("UserController")
            ),
            (
                "list",
                NodeKind::Method,
                (6, 4, 7, 39),
                Some("UserController")
            ),
        ]
    );
    let decorators: Vec<&[String]> = g.nodes.iter().map(|n| n.decorators.as_slice()).collect();
    assert_eq!(
        decorators,
        [
            &["@RestController".to_string()][..],
            &[][..],
            &["@GetMapping(\"/users\")".to_string()][..],
        ]
    );
    let exported: Vec<bool> = g.nodes.iter().map(|n| n.is_exported).collect();
    assert_eq!(exported, [true, false, true]);
    assert_eq!(
        node(&g, "repo", NodeKind::Property)
            .type_annotation
            .as_deref(),
        Some("UserRepo")
    );
    assert_eq!(
        refs(&g),
        [
            (
                "UserController",
                "UserRepo",
                "spring-autowired",
                SPRING_AUTOWIRED,
                (5, 12, 5, 20)
            ),
            (
                "UserController",
                "list",
                "spring-route-handler",
                SPRING_ROUTE,
                (7, 18, 7, 22)
            ),
        ]
    );
}

/// Annotations alone are no proof of Spring: with no `org.springframework`
/// import the gate drops both pending refs, and the nodes stay.
#[test]
fn test_java_driver_spring_rest_controller_without_import_emits_no_refs() {
    let g = parse(SPRING_CONTROLLER.as_bytes());
    assert!(g.framework_refs.is_empty(), "{:?}", refs(&g));
    let names: Vec<&str> = g.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["UserController", "repo", "list"]);
    assert_eq!(
        node(&g, "list", NodeKind::Method).decorators,
        ["@GetMapping(\"/users\")"]
    );
}

/// Java's accepted kinds are Method and Constructor; the tx bind after
/// owner stamping reads the same indexes, and every member is stamped.
#[test]
fn test_java_driver_transactional_method_and_constructor_bind_tx_scopes() {
    let g = parse(
        b"class Svc {\n    @Transactional\n    public void save() {}\n    @Transactional\n    Svc() {}\n    void plain() {}\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("Svc", NodeKind::Class, (0, 0, 6, 1), None),
            ("save", NodeKind::Method, (1, 4, 2, 25), Some("Svc")),
            ("Svc", NodeKind::Constructor, (3, 4, 4, 12), Some("Svc")),
            ("plain", NodeKind::Method, (5, 4, 5, 19), Some("Svc")),
        ]
    );
    assert_eq!(
        tx(&g),
        [
            ("save", FrameworkId::SpringTransactional),
            ("Svc", FrameworkId::SpringTransactional),
        ]
    );
    let idx: Vec<u32> = g
        .tx_scopes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.node_idx())
        .collect();
    assert_eq!(idx, [1, 2]);
    assert_eq!(
        node(&g, "save", NodeKind::Method).decorators,
        ["@Transactional"]
    );
}

/// The component pattern's captures start inside the record header, so its
/// matches come before the record's own match.
#[test]
fn test_java_driver_record_emits_components_before_class() {
    let g = parse(b"public record Point(int x, int y) {}\n");
    assert_eq!(
        rows(&g),
        [
            ("x", NodeKind::Property, (0, 20, 0, 25), Some("Point")),
            ("y", NodeKind::Property, (0, 27, 0, 32), Some("Point")),
            ("Point", NodeKind::Class, (0, 0, 0, 36), None),
        ]
    );
    let point = node(&g, "Point", NodeKind::Class);
    assert!(point.is_exported);
    assert_eq!(
        point.content_hash,
        xxh3_64_bytes(b"public record Point(int x, int y) {}")
    );
    assert_eq!(
        node(&g, "x", NodeKind::Property).type_annotation.as_deref(),
        Some("int")
    );
}

/// Only the lambda whose body holds a call becomes `<anonymous>`; the pure
/// transform `x -> x + 1` stays out.
#[test]
fn test_java_driver_lambda_argument_with_call_emits_one_anonymous() {
    let g = parse(
        b"class A {\n    void f() {\n        run(() -> g());\n        run(x -> x + 1);\n    }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 5, 1), None),
            ("f", NodeKind::Method, (1, 4, 4, 5), Some("A")),
            (
                "<anonymous:3:12>",
                NodeKind::Function,
                (2, 12, 2, 21),
                Some("A")
            ),
        ]
    );
    let anon = &g.nodes[2];
    assert_eq!(anon.content_hash, xxh3_64_bytes(b"() -> g()"));
    assert!(!anon.is_exported && anon.type_annotation.is_none());
    assert!(anon.decorators.is_empty() && anon.heritage.is_empty());
}
