//! `JavaScriptProvider` on the shared driver, for the inputs the corpus dump
//! may not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! source position.

mod driver_parity_support;

use driver_parity_support::{assert_empty_graph, refs};
use ecp_analyzer::framework_confidence::EXPRESS_ROUTE;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph, RawExportAlias, RawNode};
use ecp_core::contracts::raw_records::{RawEndpoint, Unbound};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span = (u32, u32, u32, u32);

fn parse(src: &[u8]) -> LocalGraph {
    JavaScriptProvider::new()
        .expect("JavaScriptProvider::new")
        .parse_file(Path::new("a.js"), src)
        .expect("parse_file")
}

/// `(name, kind, span, is_exported)`
type NodeRow<'g> = (&'g str, NodeKind, Span, bool);

fn rows(g: &LocalGraph) -> Vec<NodeRow<'_>> {
    g.nodes
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.span, n.is_exported))
        .collect()
}

fn named<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node {name}: {:?}", rows(g)))
}

/// `(source, imported_name, alias, binding_kind)`
type ImportRow<'g> = (&'g str, &'g str, Option<&'g str>, Option<BindingKind>);

fn imports(g: &LocalGraph) -> Vec<ImportRow<'_>> {
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

fn blind(g: &LocalGraph) -> Vec<(&str, Span)> {
    g.blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span))
        .collect()
}

#[test]
fn test_javascript_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_empty_graph(&g, "a.js");
}

/// Imports keep match order (no sort); the binding marks run after the
/// query: the default import becomes `EsDefault`, the `export { } from`
/// re-export is `EsReexport` at emit time, and the star forms stay `None`.
#[test]
fn test_javascript_driver_imports_only_file_keeps_order_and_binding_kinds() {
    let g = parse(
        b"import { a, b as c } from './m';\nimport d from 'lib';\nimport * as ns from 'pkg';\nexport { x as y } from './re';\nexport * from './all';\nexport * as sub from './sub';\n",
    );
    assert!(g.nodes.is_empty(), "{:?}", rows(&g));
    assert_eq!(
        imports(&g),
        [
            ("./m", "a", None, None),
            ("./m", "b", Some("c"), None),
            ("lib", "d", None, Some(BindingKind::EsDefault)),
            ("pkg", "*", Some("ns"), None),
            ("./re", "x", Some("y"), Some(BindingKind::EsReexport)),
            ("./all", "*", None, None),
            ("./sub", "*", Some("sub"), None),
        ]
    );
    assert!(g.export_aliases.is_none() && g.blind_spots.is_empty());
}

/// A literal `require` is an import and no blind spot; a non-literal
/// `require` or `import()` is a conditional blind spot. The binding
/// `const fs = require(..)` is also a module-level Const.
#[test]
fn test_javascript_driver_require_literal_imports_and_dynamic_blind_spots() {
    let g = parse(
        b"const fs = require('fs');\nconst { a, b: c } = require('./lib');\nconst dyn = require(name);\nimport(spec);\nimport('./lit');\n",
    );
    assert_eq!(
        imports(&g),
        [
            ("fs", "*", Some("fs"), None),
            ("./lib", "a", None, None),
            ("./lib", "b", Some("c"), None),
        ]
    );
    assert_eq!(
        rows(&g),
        [
            ("fs", NodeKind::Const, (0, 0, 0, 25), false),
            ("dyn", NodeKind::Const, (2, 0, 2, 26), false),
        ]
    );
    assert_eq!(
        named(&g, "fs").content_hash,
        xxh3_64_bytes(b"const fs = require('fs');")
    );
    assert_eq!(
        blind(&g),
        [
            ("js-dynamic-require", (2, 12, 2, 25)),
            ("js-dynamic-import", (3, 0, 3, 12)),
        ]
    );
    assert!(g.blind_spots.iter().all(|b| !b.is_test));
}

/// The two `export function f` matches merge into one node; `export const`
/// spans its declaration, not the declarator; `export { g as h }` marks `g`
/// after the query and yields the export aliases.
#[test]
fn test_javascript_driver_es_module_exports_mark_nodes_and_aliases() {
    let g = parse(
        b"export function f() {}\nexport class C {}\nexport const k = 1;\nconst local = 2;\nfunction g() {}\nexport { g as h };\n",
    );
    assert_eq!(
        rows(&g),
        [
            ("f", NodeKind::Function, (0, 7, 0, 22), true),
            ("C", NodeKind::Class, (1, 7, 1, 17), true),
            ("k", NodeKind::Const, (2, 7, 2, 19), true),
            ("local", NodeKind::Const, (3, 0, 3, 16), false),
            ("g", NodeKind::Function, (4, 0, 4, 15), true),
        ]
    );
    assert_eq!(
        named(&g, "f").content_hash,
        xxh3_64_bytes(b"function f() {}")
    );
    assert_eq!(named(&g, "k").content_hash, xxh3_64_bytes(b"const k = 1;"));
    let alias = |e: &str, l: &str| RawExportAlias {
        exported: e.into(),
        local: l.into(),
    };
    assert_eq!(
        g.export_aliases.as_deref(),
        Some(
            &[
                alias("C", "C"),
                alias("f", "f"),
                alias("h", "g"),
                alias("k", "k")
            ][..]
        )
    );
    assert!(g.imports.is_empty());
}

/// Declarations and anonymous callbacks write one span set that the
/// variable emit reads. A function-valued declarator emits a Function and
/// no Variable; its sibling `y` spans the whole declaration, a span no
/// declaration or callback holds, so it stays. The callback inside `x`'s
/// initializer has its own span and does not hide `x`.
#[test]
fn test_javascript_driver_shared_span_set_keeps_distinct_variable_spans() {
    let g = parse(b"const x = f(() => g());\nconst h = () => g(), y = 1;\n");
    assert_eq!(
        rows(&g),
        [
            ("x", NodeKind::Const, (0, 0, 0, 23), false),
            (
                "<anonymous:1:12>",
                NodeKind::Function,
                (0, 12, 0, 21),
                false
            ),
            ("h", NodeKind::Function, (1, 6, 1, 19), false),
            ("y", NodeKind::Const, (1, 0, 1, 27), false),
        ]
    );
    assert_eq!(
        named(&g, "<anonymous:1:12>").content_hash,
        xxh3_64_bytes(b"() => g()")
    );
    assert_eq!(
        named(&g, "y").content_hash,
        xxh3_64_bytes(b"const h = () => g(), y = 1;")
    );
}

/// Only a callback whose body holds a call gets an `<anonymous>` node, and
/// that node hosts the body's call.
#[test]
fn test_javascript_driver_anonymous_callback_with_call_emits_node() {
    let src: &[u8] =
        b"function outer() {\n  items.forEach(function (x) { use(x); });\n  items.map(x => x * 2);\n}\n";
    let g = parse(src);
    assert_eq!(
        rows(&g),
        [
            ("outer", NodeKind::Function, (0, 0, 3, 1), false),
            (
                "<anonymous:2:16>",
                NodeKind::Function,
                (1, 16, 1, 40),
                false
            ),
        ]
    );
    let anon = named(&g, "<anonymous:2:16>");
    assert_eq!(
        anon.content_hash,
        xxh3_64_bytes(b"function (x) { use(x); }")
    );
    assert!(anon.calls.iter().any(|c| c == "use"), "{:?}", anon.calls);
    assert!(
        !named(&g, "outer").calls.iter().any(|c| c == "use"),
        "{:?}",
        named(&g, "outer").calls
    );
}

/// Express runs the last argument: the route's handler is the inline arrow,
/// and the only ref. Its target is the bare `<anonymous>` name and its source
/// is the callback node of the same span; middleware yields no ref.
#[test]
fn test_javascript_driver_express_inline_handler_route_and_refs() {
    let g = parse(
        b"const express = require('express');\nconst app = express();\napp.get('/users', auth, (req, res) => { res.send(list()); });\n",
    );
    assert_eq!(imports(&g), [("express", "*", Some("express"), None)]);
    assert_eq!(
        rows(&g),
        [
            ("express", NodeKind::Const, (0, 0, 0, 35), false),
            ("app", NodeKind::Const, (1, 0, 1, 22), false),
            (
                "<anonymous:3:24>",
                NodeKind::Function,
                (2, 24, 2, 59),
                false
            ),
        ]
    );
    assert_eq!(g.routes.len(), 1, "{:?}", g.routes);
    let route = &g.routes[0];
    assert_eq!(
        (route.method.as_str(), route.path.as_str(), route.span),
        ("get", "/users", (2, 0, 2, 60))
    );
    assert_eq!(
        route.handler,
        Some(RawEndpoint::Unresolved(Unbound::Inline((2, 24, 2, 59))))
    );
    let receivers: Vec<(Span, &str)> = g
        .route_receivers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|r| (r.span, r.receiver.as_str()))
        .collect();
    assert_eq!(receivers, [((2, 0, 2, 60), "app")]);
    assert_eq!(
        refs(&g),
        [
            // Contract: only the last argument is the handler; `auth` is middleware.
            (
                "<anonymous:3:24>",
                "<anonymous>",
                "express-route",
                EXPRESS_ROUTE,
                (2, 24, 2, 59)
            ),
        ]
    );
}

/// A CommonJS `module.exports = { a }` exports `a`; the assigned anonymous
/// functions emit no node.
#[test]
fn test_javascript_driver_commonjs_module_exports_marks_listed_declaration() {
    let g = parse(
        b"function a() { b(); }\nmodule.exports = { a };\nmodule.exports.c = function () { a(); };\nexports.d = () => a();\n",
    );
    // Contract: `a` is reachable through `module.exports`, so it is exported.
    assert_eq!(rows(&g), [("a", NodeKind::Function, (0, 0, 0, 21), true)]);
    assert!(g.imports.is_empty() && g.export_aliases.is_none());
    assert!(g.routes.is_empty() && g.framework_refs.is_empty() && g.blind_spots.is_empty());
}

/// `Object.freeze` with two scalar pairs is a conditional blind spot; one
/// pair is not. `eval` is unconditional. Both keep capture order.
#[test]
fn test_javascript_driver_freeze_enum_needs_two_scalar_pairs() {
    let g = parse(
        b"const Color = Object.freeze({ RED: 1, BLUE: 2 });\nconst One = Object.freeze({ A: 1 });\neval(code);\n",
    );
    assert_eq!(
        blind(&g),
        [
            ("js-object-freeze-enum", (0, 0, 0, 49)),
            ("js-eval", (2, 0, 2, 10)),
        ]
    );
    assert_eq!(
        rows(&g),
        [
            ("Color", NodeKind::Const, (0, 0, 0, 49), false),
            ("One", NodeKind::Const, (1, 0, 1, 36), false),
        ]
    );
}
