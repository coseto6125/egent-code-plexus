//! Parser fixes for TypeScript: `.tsx` grammar choice and the freeze-enum span.

use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse_at(path: &str, src: &[u8]) -> LocalGraph {
    TypeScriptProvider::new()
        .expect("TypeScriptProvider::new")
        .parse_file(Path::new(path), src)
        .expect("parse_file")
}

fn node<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node {name}: {:?}", g.nodes))
}

fn freeze_spots(g: &LocalGraph) -> Vec<(u32, u32, u32, u32)> {
    g.blind_spots
        .iter()
        .filter(|b| b.kind == "ts-object-freeze-enum")
        .map(|b| b.span)
        .collect()
}

/// FU-2026-10-30-6638b684d5f7. Only the TSX grammar reads JSX text: the `'`
/// in `it's` opens a string under the TypeScript grammar and swallows the
/// call that follows on the same line.
#[test]
fn test_typescript_tsx_path_parses_jsx_with_tsx_grammar() {
    let src = b"export function App() { return <p>it's {label()}</p>; }\nfunction label() { return 'x'; }\n";
    let g = parse_at("ui/App.tsx", src);
    let app = node(&g, "App");
    assert_eq!((app.kind, app.is_exported), (NodeKind::Function, true));
    assert_eq!(app.calls, ["label"], "{:?}", g.nodes);
    assert_eq!(node(&g, "label").kind, NodeKind::Function);
}

/// FU-2026-10-30-6638b684d5f7. A call inside a JSX attribute arrow belongs
/// to a callback node, and the sibling element child call stays on `App`.
#[test]
fn test_typescript_tsx_jsx_attribute_arrow_and_child_calls_resolve() {
    let src = b"export function App() { return <Btn onClick={() => helper()}>{label()}</Btn>; }\nfunction helper() {}\nfunction label() { return 1; }\n";
    let g = parse_at("ui/App.tsx", src);
    assert_eq!(node(&g, "App").calls, ["label"], "{:?}", g.nodes);
    assert!(
        g.nodes
            .iter()
            .any(|n| n.name.starts_with("<anonymous") && n.calls == ["helper"]),
        "{:?}",
        g.nodes
    );
}

/// FU-2026-10-30-6638b684d5f7 guard: a `.ts` path keeps the TypeScript
/// grammar, so a `<T>` generic arrow is not JSX there.
#[test]
fn test_typescript_ts_path_generic_arrow_stays_typescript() {
    let g = parse_at("a.ts", b"const f = <T>(x: T) => g(x);\n");
    assert!(g.nodes.iter().any(|n| n.name == "f"), "{:?}", g.nodes);
}

/// FU-2026-10-30-6638b684d5f7 guard: a `.tsx` file with no JSX keeps the
/// same top-level nodes and exports under the TSX grammar.
#[test]
fn test_typescript_tsx_path_without_jsx_matches_ts_nodes() {
    let src = b"export function App() { return render(); }\nconst f = <T,>(x: T) => g(x);\n";
    let tsx = parse_at("ui/App.tsx", src);
    let ts = parse_at("ui/App.ts", src);
    let shape = |g: &LocalGraph| {
        g.nodes
            .iter()
            .map(|n| (n.name.clone(), n.kind, n.span, n.is_exported))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&tsx), shape(&ts));
}

/// FU-2026-10-30-6638b684d5f7. An `Object.freeze` spot spans the whole
/// declaration, as an `as const` spot and the JavaScript spot do (the
/// declaration ends after the `;`).
#[test]
fn test_typescript_freeze_enum_spot_spans_whole_declaration() {
    let g = parse_at("a.ts", b"const Status = Object.freeze({ A: 1, B: 2 });\n");
    assert_eq!(freeze_spots(&g), [(0, 0, 0, 45)]);
}

/// FU-2026-10-30-6638b684d5f7. The declaration of `export const` is the
/// inner `const` statement, for freeze and `as const` alike.
#[test]
fn test_typescript_freeze_and_as_const_spots_agree_under_export() {
    let freeze = parse_at("a.ts", b"export const S = Object.freeze({ A: 1, B: 2 });\n");
    assert_eq!(freeze_spots(&freeze), [(0, 7, 0, 47)]);
    let as_const = parse_at("a.ts", b"export const C = { R: 'r', G: 'g' } as const;\n");
    assert_eq!(freeze_spots(&as_const), [(0, 7, 0, 45)]);
}

/// FU-2026-10-30-6638b684d5f7 guard: the `as const` spot keeps spanning the
/// whole declaration, and a nested `Object.freeze` (no declaration parent)
/// keeps the call span.
#[test]
fn test_typescript_as_const_spot_and_bare_freeze_call_span_unchanged() {
    let g = parse_at("a.ts", b"const Color = { R: 'r', G: 'g' } as const;\n");
    assert_eq!(freeze_spots(&g), [(0, 0, 0, 42)]);
    let bare = parse_at("a.ts", b"use(Object.freeze({ A: 1, B: 2 }));\n");
    assert_eq!(freeze_spots(&bare), [(0, 4, 0, 33)]);
}

/// FU-2026-10-30-855c06cfb700 sibling: the TypeScript Express query also
/// takes only the last-argument identifier as the handler.
#[test]
fn test_typescript_express_middleware_identifier_gets_no_route_ref() {
    let g = parse_at(
        "a.ts",
        b"import express from 'express';\nconst app = express();\napp.get('/users', auth, listUsers);\n",
    );
    let targets: Vec<&str> = g
        .framework_refs
        .iter()
        .filter(|r| r.reason == "express-route")
        .map(|r| r.target_name.as_str())
        .collect();
    assert_eq!(targets, ["listUsers"]);
}
