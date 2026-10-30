//! `TypeScriptProvider` on the shared driver, for the inputs the corpus dump
//! may not hold. Each test pins the records the hand-written loop produced
//! before the move, in their emitted order where the order is fixed by
//! source position or by a walk.

mod driver_parity_support;

use driver_parity_support::{assert_empty_graph, names_kinds};
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, FrameworkId, LocalGraph, SchemaType};
use ecp_core::contracts::raw_records::{RawEndpoint, Unbound};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

type Span = (u32, u32, u32, u32);

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    TypeScriptProvider::new()
        .expect("TypeScriptProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn parse(src: &[u8]) -> LocalGraph {
    parse_at("a.ts", src)
}

fn blind(g: &LocalGraph) -> Vec<(&str, Span, bool)> {
    g.blind_spots
        .iter()
        .map(|b| (b.kind.as_str(), b.span, b.is_test))
        .collect()
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

/// `(name, type_class, owner_class, framework, span)`
type FieldRow<'g> = (&'g str, SchemaType, &'g str, FrameworkId, Span);

#[test]
fn test_typescript_driver_zero_byte_file_returns_empty_graph() {
    let g = parse(b"");
    assert_empty_graph(&g, "a.ts");
}

/// Imports keep query order; the binding-marks step tags the default import
/// after the query, and the `export *` star has no alias.
#[test]
fn test_typescript_driver_imports_only_file_marks_default_import() {
    let g = parse(b"import { b as c } from './m';\nimport d from './n';\nexport * from './z';\n");
    assert!(g.nodes.is_empty(), "{:?}", names_kinds(&g));
    assert_eq!(
        imports(&g),
        [
            ("./m", "b", Some("c"), None),
            ("./n", "d", None, Some(BindingKind::EsDefault)),
            ("./z", "*", None, None),
        ]
    );
    assert!(g.export_aliases.is_none() && g.event_topics.is_none());
}

/// The freeze-enum walk pops the last pushed child first, so the second
/// declaration's spot comes first. An `as const` spot spans the whole
/// declaration; an `Object.freeze` spot spans it too.
#[test]
fn test_typescript_driver_freeze_enum_spots_follow_walk_order() {
    let g = parse(
        b"const Status = Object.freeze({ A: 1, B: 2 });\nconst Color = { R: 'r', G: 'g' } as const;\n",
    );
    assert_eq!(
        blind(&g),
        [
            ("ts-object-freeze-enum", (1, 0, 1, 42), false),
            // Contract: a freeze spot spans the whole declaration, as `as const` does.
            ("ts-object-freeze-enum", (0, 0, 0, 45), false),
        ]
    );
    assert!(g
        .blind_spots
        .iter()
        .all(|b| b.file_path == Path::new("a.ts")));
    assert_eq!(
        names_kinds(&g),
        [("Status", NodeKind::Const), ("Color", NodeKind::Const)]
    );
}

/// Literal specifiers push nothing; `eval` pushes whatever its argument.
#[test]
fn test_typescript_driver_conditional_blind_spots_skip_literal_arguments() {
    let g = parse(b"import('./a');\nimport(name);\nrequire(name);\nrequire('b');\neval(code);\n");
    assert_eq!(
        blind(&g),
        [
            ("ts-dynamic-import", (1, 0, 1, 12), false),
            ("ts-dynamic-require", (2, 0, 2, 13), false),
            ("ts-eval", (4, 0, 4, 10), false),
        ]
    );
}

/// An empty schema gate is active: interface fields need no import.
#[test]
fn test_typescript_driver_interface_without_import_emits_schema_fields() {
    let g = parse(b"interface User { id: number; name: string; }\n");
    assert_eq!(names_kinds(&g), [("User", NodeKind::Interface)]);
    assert_eq!(g.nodes[0].span, (0, 0, 0, 44));
    assert!(g.imports.is_empty());
    let fields = g.schema_fields.as_deref().expect("schema_fields");
    let got: Vec<FieldRow<'_>> = fields
        .iter()
        .map(|f| (&*f.name, f.type_class, &*f.owner_class, f.framework, f.span))
        .collect();
    assert_eq!(
        got,
        [
            (
                "id",
                SchemaType::Int,
                "User",
                FrameworkId::TypeScriptInterface,
                (0, 10, 0, 14)
            ),
            (
                "name",
                SchemaType::String,
                "User",
                FrameworkId::TypeScriptInterface,
                (0, 10, 0, 14)
            ),
        ]
    );
    assert!(fields.iter().all(|f| f.endpoint.is_none()));
}

/// `export { a as b }` has no source: no import, an export alias, and the
/// local declaration is stamped exported after the query.
#[test]
fn test_typescript_driver_export_alias_stamps_local_declaration() {
    let g = parse(b"function a() {}\nexport { a as b };\n");
    assert_eq!(names_kinds(&g), [("a", NodeKind::Function)]);
    assert_eq!(
        (g.nodes[0].span, g.nodes[0].is_exported),
        ((0, 0, 0, 15), true)
    );
    assert!(g.imports.is_empty());
    let aliases: Vec<(&str, &str)> = g
        .export_aliases
        .as_deref()
        .expect("export_aliases")
        .iter()
        .map(|a| (&*a.exported, &*a.local))
        .collect();
    assert_eq!(aliases, [("b", "a")]);
}

/// Only a callback whose body holds a call gets an `<anonymous>` node.
#[test]
fn test_typescript_driver_anonymous_callback_with_call_emits_one_node() {
    let g = parse(b"register(() => { run(); });\nregister(() => 1);\n");
    assert_eq!(names_kinds(&g), [("<anonymous:1:9>", NodeKind::Function)]);
    let n = &g.nodes[0];
    assert_eq!(n.span, (0, 9, 0, 25));
    assert_eq!(n.content_hash, xxh3_64_bytes(b"() => { run(); }"));
    assert!(!n.is_exported && n.decorators.is_empty() && n.heritage.is_empty());
}

/// The route's handler is the inline arrow; a non-identifier handler stages
/// no framework ref.
#[test]
fn test_typescript_driver_express_inline_handler_route() {
    let g = parse(
        b"import express from 'express';\nconst app = express();\napp.get('/users', (req, res) => { res.send(list()); });\n",
    );
    let routes: Vec<_> = g
        .routes
        .iter()
        .map(|r| {
            (
                r.method.as_str(),
                r.path.as_str(),
                r.handler.clone(),
                r.span,
            )
        })
        .collect();
    assert_eq!(
        routes,
        [(
            "get",
            "/users",
            Some(RawEndpoint::Unresolved(Unbound::Inline((2, 18, 2, 53)))),
            (2, 0, 2, 54)
        )]
    );
    assert!(g.framework_refs.is_empty());
    assert_eq!(
        imports(&g),
        [("express", "express", None, Some(BindingKind::EsDefault))]
    );
    let anon: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("<anonymous"))
        .map(|n| (n.name.as_str(), n.span))
        .collect();
    assert_eq!(anon, [("<anonymous:3:18>", (2, 18, 2, 53))]);
}

/// The `@const` match of a function-valued binding ends early, so only the
/// `@function` match's node is emitted.
#[test]
fn test_typescript_driver_arrow_const_emits_function_only() {
    let g = parse(b"const f = (x: number) => g(x);\n");
    assert_eq!(names_kinds(&g), [("f", NodeKind::Function)]);
    assert_eq!(g.nodes[0].span, (0, 6, 0, 29));
    assert_eq!(
        g.nodes[0].content_hash,
        xxh3_64_bytes(b"f = (x: number) => g(x)")
    );
}

/// A `.tsx` path parses with the TSX grammar; a JSX-free file with the
/// `<T,>` generic form gives the same nodes under both extensions.
#[test]
fn test_typescript_driver_tsx_file_without_jsx_matches_typescript_grammar() {
    // Contract: `<T,>` is the TSX-legal generic arrow, so the source has one reading.
    let src: &[u8] = b"export function App() { return render(); }\nconst f = <T,>(x: T) => g(x);\n";
    let tsx = parse_at("ui/App.tsx", src);
    let app = &tsx.nodes[0];
    assert_eq!((app.name.as_str(), app.kind), ("App", NodeKind::Function));
    assert_eq!((app.span, app.is_exported), ((0, 7, 0, 42), true));
    assert_eq!(
        app.content_hash,
        xxh3_64_bytes(b"function App() { return render(); }")
    );
    let ts = parse_at("ui/App.ts", src);
    assert_eq!(format!("{:?}", tsx.nodes), format!("{:?}", ts.nodes));
    assert_eq!(format!("{:?}", tsx.imports), format!("{:?}", ts.imports));
}
