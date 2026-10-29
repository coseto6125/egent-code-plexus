//! `GoProvider` on the shared driver, for the inputs the corpus dump may
//! not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! source position.

use ecp_analyzer::framework_confidence::{ECHO_ROUTE, GIN_ROUTE};
use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

fn parse(src: &[u8]) -> LocalGraph {
    GoProvider::new()
        .expect("GoProvider::new")
        .parse_file(Path::new("a.go"), src)
        .expect("parse_file")
}

fn names_kinds(g: &LocalGraph) -> Vec<(&str, NodeKind)> {
    g.nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

fn of_kind(g: &LocalGraph, kind: NodeKind) -> Vec<&RawNode> {
    g.nodes.iter().filter(|n| n.kind == kind).collect()
}

/// `(source_name, target_name, reason, confidence, span)`
type Ref<'g> = (&'g str, &'g str, &'g str, f32, (u32, u32, u32, u32));

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

/// `(method, path, handler name, span)`
type RouteRow<'g> = (&'g str, &'g str, Option<&'g str>, (u32, u32, u32, u32));

fn routes(g: &LocalGraph) -> Vec<RouteRow<'_>> {
    g.routes
        .iter()
        .map(|r| (r.method.as_str(), r.path.as_str(), r.handler_name(), r.span))
        .collect()
}

#[test]
fn test_go_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_eq!(g.file_path, Path::new("a.go"));
    assert!(g.nodes.is_empty() && g.imports.is_empty() && g.blind_spots.is_empty());
    assert!(g.framework_refs.is_empty() && g.call_metas.is_empty());
    assert!(g.raw_function_metas.is_empty() && g.routes.is_empty() && g.fanout_refs.is_empty());
    assert!(g.event_topics.is_none() && g.tx_scopes.is_none() && g.schema_fields.is_none());
    assert!(g.path_literals.is_none() && g.sql_refs.is_none());
    assert!(g.route_receivers.is_none() && g.export_aliases.is_none());
}

/// `_` and `.` bind no package name, so both specs key on `(source, "x")`;
/// the stable sort keeps the first alias in source order.
#[test]
fn test_go_driver_imports_only_file_keeps_first_alias_of_duplicate() {
    let g = parse(b"package p\n\nimport (\n\t\"example.com/z\"\n\t_ \"example.com/x\"\n\t. \"example.com/x\"\n)\n");
    assert_eq!(names_kinds(&g), [("p", NodeKind::Namespace)]);
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
            ("example.com/x", "x", Some("_")),
            ("example.com/z", "z", None)
        ]
    );
    assert!(g.imports.iter().all(|i| i.binding_kind.is_none()));
    assert!(g.event_topics.is_none());
}

/// The iota walk runs before the query, so its blind spot is the whole
/// `const_declaration`, pushed once for the block.
#[test]
fn test_go_driver_iota_const_block_emits_one_blind_spot() {
    let g = parse(b"package p\n\nconst (\n\tA = iota\n\tB\n)\n");
    assert_eq!(g.blind_spots.len(), 1, "{:?}", g.blind_spots);
    let b = &g.blind_spots[0];
    assert_eq!(b.kind, "go-iota-const-block");
    assert_eq!((b.span, b.is_test), ((2, 0, 5, 1), false));
    assert_eq!(b.file_path, Path::new("a.go"));
    let consts: Vec<&str> = of_kind(&g, NodeKind::Const)
        .into_iter()
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(consts, ["A", "B"]);
}

/// The typed `@var` Variable is pushed in the query; the untyped one comes
/// from the pending merge after it. The pending copy of `X` has the same
/// name and span, so the merge drops it.
#[test]
fn test_go_driver_typed_and_untyped_file_var_emit_one_variable_each() {
    let g = parse(b"package p\n\nvar X int = 1\nvar y = 2\n");
    let vars = of_kind(&g, NodeKind::Variable);
    let got: Vec<_> = vars
        .iter()
        .map(|n| {
            (
                n.name.as_str(),
                n.type_annotation.as_deref(),
                n.span,
                n.is_exported,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("X", Some("int"), (2, 4, 2, 13), true),
            ("y", None, (3, 4, 3, 9), false),
        ]
    );
    assert_eq!(vars[0].content_hash, xxh3_64_bytes(b"X int = 1"));
    assert_eq!(vars[1].content_hash, xxh3_64_bytes(b"y = 2"));
}

/// `_` skips its own name only; the other names of the same declaration stay.
#[test]
fn test_go_driver_blank_var_name_skips_only_blank() {
    let g = parse(b"package p\n\nvar _, Z int\n\nfunc f() {\n\t_, err := g()\n}\n");
    assert!(
        !g.nodes.iter().any(|n| n.name == "_"),
        "{:?}",
        names_kinds(&g)
    );
    let vars = of_kind(&g, NodeKind::Variable);
    let got: Vec<_> = vars
        .iter()
        .map(|n| {
            (
                n.name.as_str(),
                n.type_annotation.as_deref(),
                n.span,
                n.owner_class.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Z", Some("int"), (2, 7, 2, 12), None),
            ("err", None, (5, 4, 5, 14), Some("f")),
        ]
    );
    assert_eq!(vars[0].content_hash, xxh3_64_bytes(b"_, Z int"));
    assert_eq!(vars[1].content_hash, xxh3_64_bytes(b"_, err := g()"));
}

/// gin runs the last argument; `auth` is middleware.
#[test]
fn test_go_driver_gin_route_last_argument_is_handler() {
    let g = parse(b"package p\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc setup(r *gin.Engine) {\n\tr.GET(\"/ping\", auth, ping)\n}\n");
    assert_eq!(routes(&g), [("GET", "/ping", Some("ping"), (5, 1, 5, 27))]);
    assert_eq!(
        refs(&g),
        [("setup", "ping", "gin-route", GIN_ROUTE, (5, 22, 5, 26))]
    );
}

/// echo takes the argument after the path; the route inherits its group prefix.
#[test]
fn test_go_driver_echo_route_group_prefixes_path() {
    let g = parse(b"package p\n\nimport \"github.com/labstack/echo/v4\"\n\nfunc setup(e *echo.Echo) {\n\tg := e.Group(\"/v1\")\n\tg.POST(\"/users\", create, mw)\n}\n");
    assert_eq!(
        routes(&g),
        [("POST", "/v1/users", Some("create"), (6, 1, 6, 29))]
    );
    assert_eq!(
        refs(&g),
        [("setup", "create", "echo-route", ECHO_ROUTE, (6, 18, 6, 24))]
    );
}

#[test]
fn test_go_driver_pointer_receiver_method_owns_receiver_type() {
    let src: &[u8] = b"package p\n\ntype Dog struct{}\n\nfunc (d *Dog) Bark() {}\n";
    let g = parse(src);
    let methods = of_kind(&g, NodeKind::Method);
    assert_eq!(methods.len(), 1, "{:?}", names_kinds(&g));
    let m = methods[0];
    assert_eq!(m.name, "Bark");
    assert_eq!(m.owner_class.as_deref(), Some("Dog"));
    assert_eq!(m.type_annotation.as_deref(), Some("Dog"));
    assert_eq!((m.span, m.is_exported), ((4, 0, 4, 23), true));
    assert!(m.decorators.is_empty() && m.heritage.is_empty());
    assert_eq!(m.content_hash, xxh3_64_bytes(b"func (d *Dog) Bark() {}"));
}
