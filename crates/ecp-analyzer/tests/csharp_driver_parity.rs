//! `CSharpProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order: query match order, which is
//! source order for these inputs.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::framework_confidence::ASPNET_HINT;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawNode, OVERRIDE_SENTINEL};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span4 = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    CSharpProvider::new()
        .expect("CSharpProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("A.cs", src)
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

fn exported(g: &LocalGraph) -> Vec<bool> {
    g.nodes.iter().map(|n| n.is_exported).collect()
}

fn types(g: &LocalGraph) -> Vec<Option<&str>> {
    g.nodes
        .iter()
        .map(|n| n.type_annotation.as_deref())
        .collect()
}

fn decorators(g: &LocalGraph) -> Vec<&[String]> {
    g.nodes.iter().map(|n| n.decorators.as_slice()).collect()
}

/// `(source_name, target_name, reason, confidence, span)`
type Ref<'g> = (&'g str, &'g str, &'g str, f32, Span4);

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
fn test_csharp_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("A.cs"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// The three `using` patterns overlap and C# never dedupes: the alias form
/// also matches the identifier pattern (on the alias) and the qualified-name
/// pattern (without the alias).
#[test]
fn test_csharp_driver_imports_only_file_keeps_overlapping_using_matches() {
    let g = parse(
        b"using System;\nusing System.Collections.Generic;\nusing static System.Math;\nusing Json = Newtonsoft.Json;\n",
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
            ("System", "System", None),
            (
                "System.Collections.Generic",
                "System.Collections.Generic",
                None
            ),
            ("System.Math", "System.Math", None),
            ("Json", "Json", None),
            ("Newtonsoft.Json", "Newtonsoft.Json", None),
            ("Newtonsoft.Json", "Newtonsoft.Json", Some("Json")),
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.framework_refs.is_empty() && g.tx_scopes.is_none());
}

/// `private int a, b;` keys its dedupe on each name, so two Property nodes
/// share the declaration's span and hash. Only a `public` modifier exports.
/// The class's tightest owner is the namespace; members name the class.
#[test]
fn test_csharp_driver_namespace_class_property_and_method_emit_in_order() {
    let g = parse(
        b"namespace App.Core\n{\n    public class Dog\n    {\n        public string Name { get; set; }\n        private int a, b;\n        public void Bark() { Sound(); }\n    }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("App.Core", NodeKind::Namespace, (0, 0, 8, 1), None),
            ("Dog", NodeKind::Class, (2, 4, 7, 5), Some("App.Core")),
            ("Name", NodeKind::Property, (4, 8, 4, 40), Some("Dog")),
            ("a", NodeKind::Property, (5, 8, 5, 25), Some("Dog")),
            ("b", NodeKind::Property, (5, 8, 5, 25), Some("Dog")),
            ("Bark", NodeKind::Method, (6, 8, 6, 39), Some("Dog")),
        ]
    );
    assert_eq!(exported(&g), [false, true, true, false, false, true]);
    assert_eq!(
        types(&g),
        [None, None, Some("string"), None, None, Some("void")]
    );
    assert_eq!(
        g.nodes[2].content_hash,
        xxh3_64_bytes(b"public string Name { get; set; }")
    );
    assert_eq!(g.nodes[3].content_hash, xxh3_64_bytes(b"private int a, b;"));
    assert_eq!(g.nodes[4].content_hash, xxh3_64_bytes(b"private int a, b;"));
    assert!(g
        .nodes
        .iter()
        .all(|n| n.decorators.is_empty() && n.heritage.is_empty()));
    assert!(g.tx_scopes.is_none() && g.blind_spots.is_empty());
}

/// C#'s accepted tx kinds are Method, Constructor and Function; both the bare
/// and the parameterised attribute forms bind, and the plain method does not.
#[test]
fn test_csharp_driver_transactional_method_and_constructor_bind_tx_scopes() {
    let g = parse(
        b"public class OrderService\n{\n    [Transactional]\n    public void PlaceOrder() {}\n    [TransactionAttribute(Timeout = 5)]\n    public OrderService() {}\n    public void ListOrders() {}\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("OrderService", NodeKind::Class, (0, 0, 7, 1), None),
            (
                "PlaceOrder",
                NodeKind::Method,
                (2, 4, 3, 31),
                Some("OrderService")
            ),
            (
                "OrderService",
                NodeKind::Constructor,
                (4, 4, 5, 28),
                Some("OrderService")
            ),
            (
                "ListOrders",
                NodeKind::Method,
                (6, 4, 6, 31),
                Some("OrderService")
            ),
        ]
    );
    assert_eq!(
        tx(&g),
        [
            ("PlaceOrder", FrameworkId::DotNetTransactional),
            ("OrderService", FrameworkId::DotNetTransactional),
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
        decorators(&g),
        [
            &[][..],
            &["[Transactional]".to_string()][..],
            &["[TransactionAttribute(Timeout = 5)]".to_string()][..],
            &[][..],
        ]
    );
    assert_eq!(
        g.nodes[1].content_hash,
        xxh3_64_bytes(b"[Transactional]\n    public void PlaceOrder() {}")
    );
}

/// A lambda or anonymous delegate becomes `<anonymous>` only when its body
/// holds an invocation; the pure transform `x => x + 1` stays out.
#[test]
fn test_csharp_driver_lambda_and_delegate_with_call_emit_anonymous() {
    let g = parse(
        b"class A\n{\n    void F()\n    {\n        Run(() => Go());\n        Run(x => x + 1);\n        Run(delegate { Go(); });\n    }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("A", NodeKind::Class, (0, 0, 8, 1), None),
            ("F", NodeKind::Method, (2, 4, 7, 5), Some("A")),
            (
                "<anonymous:5:12>",
                NodeKind::Function,
                (4, 12, 4, 22),
                Some("A")
            ),
            (
                "<anonymous:7:12>",
                NodeKind::Function,
                (6, 12, 6, 30),
                Some("A")
            ),
        ]
    );
    assert_eq!(g.nodes[2].content_hash, xxh3_64_bytes(b"() => Go()"));
    assert_eq!(
        g.nodes[3].content_hash,
        xxh3_64_bytes(b"delegate { Go(); }")
    );
    for anon in &g.nodes[2..] {
        assert!(!anon.is_exported && anon.type_annotation.is_none());
        assert!(anon.decorators.is_empty() && anon.heritage.is_empty());
    }
}

/// A partial generic type carries its arity marker, then its partial marker
/// with the declaring scope; the nested class is owned by the outer one. A
/// file-scoped namespace spells the scope too, but spans only its own line.
#[test]
fn test_csharp_driver_partial_generic_and_nested_class_emit_markers_and_owner() {
    let g = parse(
        b"namespace N\n{\n    public partial class Outer<T>\n    {\n        class Inner {}\n    }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("N", NodeKind::Namespace, (0, 0, 6, 1), None),
            ("Outer", NodeKind::Class, (2, 4, 5, 5), Some("N")),
            ("Inner", NodeKind::Class, (4, 8, 4, 22), Some("Outer")),
        ]
    );
    assert_eq!(
        decorators(&g),
        [
            &[][..],
            &[
                RawNode::generic_arity_marker(1),
                RawNode::partial_marker("N")
            ][..],
            &[][..],
        ]
    );
    assert_eq!(exported(&g), [false, true, false]);

    let g = parse(b"namespace Lib.Json;\npublic partial class W {}\n");
    assert_eq!(
        rows(&g),
        [
            ("Lib.Json", NodeKind::Namespace, (0, 0, 0, 19), None),
            ("W", NodeKind::Class, (1, 0, 1, 25), None),
        ]
    );
    assert_eq!(
        node(&g, "W", NodeKind::Class).decorators,
        [RawNode::partial_marker("Lib.Json")]
    );
}

/// The parser emits no routes: an ASP.NET attribute only feeds the text
/// framework detector, whose ref spans the first pattern found.
#[test]
fn test_csharp_driver_aspnet_route_attribute_emits_framework_ref_not_route() {
    let g = parse(
        b"[ApiController]\npublic class UsersController : ControllerBase\n{\n    [HttpGet(\"users\")]\n    public string List() { return \"\"; }\n}\n",
    );
    assert!(g.routes.is_empty() && g.route_receivers.is_none());
    assert_eq!(
        rows(&g),
        [
            ("UsersController", NodeKind::Class, (0, 0, 5, 1), None),
            (
                "List",
                NodeKind::Method,
                (3, 4, 4, 39),
                Some("UsersController")
            ),
        ]
    );
    assert_eq!(
        decorators(&g),
        [
            &["[ApiController]".to_string()][..],
            &["[HttpGet(\"users\")]".to_string()][..],
        ]
    );
    assert_eq!(
        node(&g, "UsersController", NodeKind::Class).heritage,
        ["ControllerBase"]
    );
    let refs: Vec<Ref<'_>> = g
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
            "<module>",
            "aspnet",
            "aspnet-attribute",
            ASPNET_HINT,
            (0, 0, 0, 15)
        )]
    );
}

/// Both reflective calls push one blind spot each, in capture order; a test
/// path marks them as test scaffolding.
#[test]
fn test_csharp_driver_reflection_calls_emit_blind_spots() {
    let src = b"class R\n{\n    void M(Type t, MethodInfo m)\n    {\n        Activator.CreateInstance(t);\n        m.Invoke(this, null);\n    }\n}\n";
    for (path, is_test) in [("A.cs", false), ("src/MyApp/UserTest.cs", true)] {
        let g = parse_at(path, src);
        let blinds: Vec<(&str, Span4, bool)> = g
            .blind_spots
            .iter()
            .map(|b| (b.kind.as_str(), b.span, b.is_test))
            .collect();
        assert_eq!(
            blinds,
            [
                ("cs-activator-create-instance", (4, 8, 4, 35), is_test),
                ("cs-method-invoke", (5, 8, 5, 28), is_test),
            ],
            "{path}"
        );
        assert!(g.blind_spots.iter().all(|b| b.file_path == Path::new(path)));
        assert!(g.blind_spots[0]
            .hint
            .starts_with("Activator.CreateInstance(<expr>)"));
        assert!(g.blind_spots[1].hint.starts_with("<expr>.Invoke(...)"));
        assert_eq!(
            rows(&g),
            [
                ("R", NodeKind::Class, (0, 0, 7, 1), None),
                ("M", NodeKind::Method, (2, 4, 6, 5), Some("R")),
            ]
        );
    }
}

/// The special members: a destructor gets the `~` prefix, an event field
/// emits one Method per declarator, operators and the indexer get
/// synthesised names, `override` adds the sentinel, and a class whose base
/// ends in `Attribute` becomes an Annotation.
#[test]
fn test_csharp_driver_special_members_emit_synthesised_methods() {
    let g = parse(
        b"public class TagAttribute : Attribute {}\nclass V : Base\n{\n    ~V() { Free(); }\n    public event EventHandler Click, Hover;\n    public static V operator +(V a, V b) { return a; }\n    public static implicit operator int(V v) { return 0; }\n    public int this[int i] { get { return i; } }\n    public override string ToString() { return \"\"; }\n}\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("TagAttribute", NodeKind::Annotation, (0, 0, 0, 40), None),
            ("V", NodeKind::Class, (1, 0, 9, 1), None),
            ("~V", NodeKind::Method, (3, 4, 3, 20), Some("V")),
            ("Click", NodeKind::Method, (4, 4, 4, 43), Some("V")),
            ("Hover", NodeKind::Method, (4, 4, 4, 43), Some("V")),
            ("op_+", NodeKind::Method, (5, 4, 5, 54), Some("V")),
            ("op_Implicit", NodeKind::Method, (6, 4, 6, 58), Some("V")),
            ("this[...]", NodeKind::Method, (7, 4, 7, 48), Some("V")),
            ("ToString", NodeKind::Method, (8, 4, 8, 52), Some("V")),
        ]
    );
    assert_eq!(
        exported(&g),
        [true, false, false, true, true, true, true, true, true]
    );
    assert_eq!(
        types(&g),
        [
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("int"),
            Some("string")
        ]
    );
    let heritage: Vec<&[String]> = g.nodes.iter().map(|n| n.heritage.as_slice()).collect();
    assert_eq!(heritage[0], ["Attribute"]);
    assert_eq!(heritage[1], ["Base"]);
    assert!(heritage[2..].iter().all(|h| h.is_empty()));
    assert_eq!(
        node(&g, "ToString", NodeKind::Method).decorators,
        [OVERRIDE_SENTINEL]
    );
    assert!(g.nodes[..8].iter().all(|n| n.decorators.is_empty()));
    assert_eq!(g.nodes[2].content_hash, xxh3_64_bytes(b"~V() { Free(); }"));
    assert_eq!(
        g.nodes[3].content_hash,
        xxh3_64_bytes(b"public event EventHandler Click, Hover;")
    );
    assert_eq!(g.nodes[3].content_hash, g.nodes[4].content_hash);
    assert_eq!(
        g.nodes[7].content_hash,
        xxh3_64_bytes(b"public int this[int i] { get { return i; } }")
    );
}
