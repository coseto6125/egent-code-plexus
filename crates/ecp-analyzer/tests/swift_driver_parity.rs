//! `SwiftProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move; names, kinds, spans, owners, imports, framework refs and
//! blind spots were read off a `LocalGraph` dump the pre-move binary wrote
//! from the same sources.

use ecp_analyzer::framework_confidence::{SWIFTUI_HINT, UIKIT_HINT, VAPOR_HINT};
use ecp_analyzer::framework_helpers::MODULE_LEVEL_SOURCE;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span4 = (u32, u32, u32, u32);

fn parse(src: &[u8]) -> LocalGraph {
    SwiftProvider::new()
        .expect("SwiftProvider::new")
        .parse_file(Path::new("a.swift"), src)
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

/// `(span, is_exported, owner_class, type_annotation)`
type Shape<'g> = (Span4, bool, Option<&'g str>, Option<&'g str>);

fn shape(n: &RawNode) -> Shape<'_> {
    (
        n.span,
        n.is_exported,
        n.owner_class.as_deref(),
        n.type_annotation.as_deref(),
    )
}

/// `(source, imported_name, alias)`
fn imports(g: &LocalGraph) -> Vec<(&str, &str, Option<&str>)> {
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

#[test]
fn test_swift_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.swift"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// Imports keep source order with no sort or dedupe; `UIKit` matches no
/// text-framework pattern.
#[test]
fn test_swift_driver_imports_only_file_keeps_source_order_and_no_nodes() {
    let g = parse(b"import Foundation\nimport UIKit\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    assert_eq!(
        imports(&g),
        [("Foundation", "Foundation", None), ("UIKit", "UIKit", None)]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.framework_refs.is_empty() && g.blind_spots.is_empty());
}

/// `case a, b, c` pushes one node per name in capture order; all three share
/// the `enum_entry` span and its hash. Each other line is its own entry.
#[test]
fn test_swift_driver_enum_cases_one_line_share_span_and_hash() {
    let g = parse(b"enum E {\n    case a, b, c\n    case d\n    case e(Int)\n}\n");
    assert_eq!(
        names_kinds(&g),
        [
            ("E", NodeKind::Enum),
            ("a", NodeKind::EnumVariant),
            ("b", NodeKind::EnumVariant),
            ("c", NodeKind::EnumVariant),
            ("d", NodeKind::EnumVariant),
            ("e", NodeKind::EnumVariant),
        ]
    );
    let e = node(&g, "E", NodeKind::Enum);
    assert_eq!(shape(e), ((0, 0, 4, 1), false, None, None));
    let one_line = xxh3_64_bytes(b"case a, b, c");
    for name in ["a", "b", "c"] {
        let v = node(&g, name, NodeKind::EnumVariant);
        assert_eq!(shape(v), ((1, 4, 1, 16), true, Some("E"), None), "{name}");
        assert_eq!(v.content_hash, one_line, "{name}");
    }
    let d = node(&g, "d", NodeKind::EnumVariant);
    assert_eq!(shape(d), ((2, 4, 2, 10), true, Some("E"), None));
    assert_eq!(d.content_hash, xxh3_64_bytes(b"case d"));
    let e = node(&g, "e", NodeKind::EnumVariant);
    assert_eq!(shape(e), ((3, 4, 3, 15), true, Some("E"), None));
    assert_eq!(e.content_hash, xxh3_64_bytes(b"case e(Int)"));
}

#[test]
fn test_swift_driver_class_method_and_property_owned_by_class() {
    let src: &[u8] =
        b"public class Box: Base {\n    var size: Int = 0\n    func grow() {\n        size += 1\n    }\n}\n";
    let g = parse(src);
    assert_eq!(
        names_kinds(&g),
        [
            ("Box", NodeKind::Class),
            ("size", NodeKind::Property),
            ("grow", NodeKind::Method),
        ]
    );
    let class = node(&g, "Box", NodeKind::Class);
    assert_eq!(shape(class), ((0, 0, 5, 1), true, None, None));
    assert_eq!(class.heritage, ["Base"]);
    assert_eq!(class.content_hash, xxh3_64_bytes(&src[..src.len() - 1]));
    let size = node(&g, "size", NodeKind::Property);
    assert_eq!(shape(size), ((1, 4, 1, 21), true, Some("Box"), Some("Int")));
    assert_eq!(size.content_hash, xxh3_64_bytes(b"var size: Int = 0"));
    let grow = node(&g, "grow", NodeKind::Method);
    assert_eq!(shape(grow), ((2, 4, 4, 5), false, Some("Box"), None));
    assert!(grow.decorators.is_empty() && grow.heritage.is_empty());
}

/// The extension emits no type node. Its method lies outside the struct's
/// span, so only the member-owner hook (before the span stamp) owns it.
#[test]
fn test_swift_driver_extension_method_owned_by_extended_struct() {
    let src: &[u8] = b"struct Point {\n    var x: Int\n}\n\nextension Point {\n    func norm() -> Int {\n        return x\n    }\n}\n";
    let g = parse(src);
    assert_eq!(
        names_kinds(&g),
        [
            ("Point", NodeKind::Struct),
            ("x", NodeKind::Property),
            ("norm", NodeKind::Method),
        ]
    );
    let point = node(&g, "Point", NodeKind::Struct);
    assert_eq!(shape(point), ((0, 0, 2, 1), false, None, None));
    let x = node(&g, "x", NodeKind::Property);
    assert_eq!(shape(x), ((1, 4, 1, 14), true, Some("Point"), Some("Int")));
    let norm = node(&g, "norm", NodeKind::Method);
    assert_eq!(shape(norm), ((5, 4, 7, 5), false, Some("Point"), None));
    assert_eq!(
        norm.content_hash,
        xxh3_64_bytes(b"func norm() -> Int {\n        return x\n    }")
    );
}

#[test]
fn test_swift_driver_protocol_emits_trait_with_owned_requirements() {
    let g = parse(b"protocol Shape {\n    var area: Double { get }\n    func draw()\n}\n");
    assert_eq!(
        names_kinds(&g),
        [
            ("Shape", NodeKind::Trait),
            ("area", NodeKind::Property),
            ("draw", NodeKind::Method),
        ]
    );
    let shape_trait = node(&g, "Shape", NodeKind::Trait);
    assert_eq!(shape(shape_trait), ((0, 0, 3, 1), false, None, None));
    let area = node(&g, "area", NodeKind::Property);
    assert_eq!(
        shape(area),
        ((1, 4, 1, 28), true, Some("Shape"), Some("Double"))
    );
    let draw = node(&g, "draw", NodeKind::Method);
    assert_eq!(shape(draw), ((2, 4, 2, 15), false, Some("Shape"), None));
}

/// A trailing and an arg-position closure that hold a call each get one
/// `<anonymous>` node; a closure with no call gets none.
#[test]
fn test_swift_driver_closure_with_call_emits_anonymous_node() {
    let g = parse(
        b"func setup() {}\n\nfetch { r in handle(r) }\nitems.map({ x in f(x) })\nitems.map { $0 + 1 }\n",
    );
    assert_eq!(
        names_kinds(&g),
        [
            ("setup", NodeKind::Function),
            ("<anonymous:3:6>", NodeKind::Function),
            ("<anonymous:4:10>", NodeKind::Function),
        ]
    );
    let trailing = node(&g, "<anonymous:3:6>", NodeKind::Function);
    assert_eq!(shape(trailing), ((2, 6, 2, 24), false, None, None));
    assert_eq!(trailing.content_hash, xxh3_64_bytes(b"{ r in handle(r) }"));
    let arg = node(&g, "<anonymous:4:10>", NodeKind::Function);
    assert_eq!(shape(arg), ((3, 10, 3, 23), false, None, None));
    assert_eq!(arg.content_hash, xxh3_64_bytes(b"{ x in f(x) }"));
}

/// Each spec reports the first of its patterns, in list order, that occurs
/// anywhere in the file; the specs report in uikit, swiftui, vapor order.
#[test]
fn test_swift_driver_text_frameworks_emit_in_spec_order() {
    let g = parse(b"import Vapor\n\n@main\nstruct App {\n    func viewDidLoad() {}\n}\n");
    let m = MODULE_LEVEL_SOURCE;
    assert_eq!(
        refs(&g),
        [
            (m, "uikit", "uikit-lifecycle", UIKIT_HINT, (4, 9, 4, 20)),
            (m, "swiftui", "swiftui-pattern", SWIFTUI_HINT, (2, 0, 2, 5)),
            (m, "vapor", "vapor-routing", VAPOR_HINT, (0, 7, 0, 12)),
        ]
    );
    assert_eq!(node(&g, "App", NodeKind::Struct).decorators, ["@main"]);
}

#[test]
fn test_swift_driver_swiftui_main_emits_swiftui_ref() {
    let g = parse(
        b"import SwiftUI\n\n@main\nstruct MyApp: App {\n    @StateObject var model = Model()\n}\n",
    );
    assert_eq!(
        refs(&g),
        [(
            MODULE_LEVEL_SOURCE,
            "swiftui",
            "swiftui-pattern",
            SWIFTUI_HINT,
            (2, 0, 2, 5)
        )]
    );
    let app = node(&g, "MyApp", NodeKind::Struct);
    assert_eq!(app.heritage, ["App"]);
    assert_eq!(app.decorators, ["@main"]);
    let model = node(&g, "model", NodeKind::Property);
    assert_eq!(shape(model), ((4, 4, 4, 36), true, Some("MyApp"), None));
}

#[test]
fn test_swift_driver_vapor_app_get_emits_vapor_ref() {
    let g = parse(b"import Vapor\n\nfunc routes(_ app: Application) {\n    app.get(\"hello\") { req in \"hi\" }\n}\n");
    assert_eq!(
        refs(&g),
        [(
            MODULE_LEVEL_SOURCE,
            "vapor",
            "vapor-routing",
            VAPOR_HINT,
            (3, 4, 3, 11)
        )]
    );
    assert_eq!(names_kinds(&g), [("routes", NodeKind::Function)]);
}

/// `viewDidLoad` is the first uikit pattern; `override` marks the method.
#[test]
fn test_swift_driver_uikit_override_emits_uikit_ref_and_sentinel() {
    let g = parse(b"class VC: UIViewController {\n    override func viewDidLoad() {}\n}\n");
    assert_eq!(
        refs(&g),
        [(
            MODULE_LEVEL_SOURCE,
            "uikit",
            "uikit-lifecycle",
            UIKIT_HINT,
            (1, 18, 1, 29)
        )]
    );
    let m = node(&g, "viewDidLoad", NodeKind::Method);
    assert_eq!(shape(m), ((1, 4, 1, 34), false, Some("VC"), None));
    assert_eq!(m.decorators, [OVERRIDE_SENTINEL]);
}

#[test]
fn test_swift_driver_blind_captures_emit_in_capture_order() {
    let g = parse(b"func load() {\n    let c = NSClassFromString(\"Foo\")\n    obj.perform(Selector(\"bar\"))\n}\n");
    let got: Vec<(&str, Span4, bool)> = g
        .blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span, b.is_test))
        .collect();
    assert_eq!(
        got,
        [
            ("swift-nsclass-from-string", (1, 12, 1, 36), false),
            ("swift-perform-selector", (2, 4, 2, 32), false),
        ]
    );
    assert!(g
        .blind_spots
        .iter()
        .all(|b| b.file_path == Path::new("a.swift")));
    assert_eq!(names_kinds(&g), [("load", NodeKind::Function)]);
}

/// Typealias and `@objc(name)` each add an alias import after their node; a
/// tuple binding emits one Variable per name on one span; a local `var` in a
/// method body emits nothing.
#[test]
fn test_swift_driver_typealias_tuple_init_objc_emit_in_match_order() {
    let g = parse(b"typealias Handler = (Int) -> Void\nlet (p, q) = (1, 2)\n\nclass Svc {\n    init(x: Int) {}\n    @objc(runNow) func run() {\n        var local = 0\n    }\n}\n");
    assert_eq!(
        names_kinds(&g),
        [
            ("Handler", NodeKind::Typedef),
            ("p", NodeKind::Variable),
            ("q", NodeKind::Variable),
            ("Svc", NodeKind::Class),
            ("init", NodeKind::Constructor),
            ("run", NodeKind::Method),
        ]
    );
    let handler = node(&g, "Handler", NodeKind::Typedef);
    assert_eq!(shape(handler), ((0, 0, 0, 33), false, None, None));
    let tuple = xxh3_64_bytes(b"let (p, q) = (1, 2)");
    for name in ["p", "q"] {
        let v = node(&g, name, NodeKind::Variable);
        assert_eq!(shape(v), ((1, 0, 1, 19), true, None, None), "{name}");
        assert_eq!(v.content_hash, tuple, "{name}");
    }
    let init = node(&g, "init", NodeKind::Constructor);
    assert_eq!(shape(init), ((4, 4, 4, 19), false, Some("Svc"), None));
    let run = node(&g, "run", NodeKind::Method);
    assert_eq!(shape(run), ((5, 4, 7, 5), false, Some("Svc"), None));
    assert_eq!(
        imports(&g),
        [
            ("(Int) -> Void", "Handler", Some("Handler")),
            ("run", "runNow", Some("runNow")),
        ]
    );
}
