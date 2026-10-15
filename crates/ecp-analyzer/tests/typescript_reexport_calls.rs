//! FU-2026-10-05-0ee319339782: a TS / JS call through a barrel module
//! (`export * from './impl'`, `export { x } from './impl'`) binds through
//! the import tier (0.95), following the re-export one hop. Before the fix
//! these calls kept the global tier: 0.7 for a unique name, no edge for an
//! ambiguous one, and the parsers emitted nothing for `export * from`.
//!
//! The guard tests pin what must not change: two star modules declaring the
//! name stay unresolved, a second hop is not followed, a cycle ends, a
//! missing re-exported name keeps the global tier, CommonJS
//! `module.exports = require()` is not a re-export, and Dart's barrel
//! resolves as before. Their expected values are what ecp 0.14.8 produced on
//! the same fixtures.
mod instantiation_calls_support;

use ecp_analyzer::dart::DartProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::{assert_call_targets, graph_of};
use std::path::Path;

fn import_tier() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn global_tier() -> f32 {
    ResolutionTier::Global.base_confidence()
}

/// Parses each file with the provider its extension names.
fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        let provider: &dyn LanguageProvider = if path.ends_with(".ts") { &ts } else { &js };
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap_or_else(|e| panic!("parse {path}: {e}")),
        );
    }
    builder.build()
}

const APP: &str = "import { render } from './index';\nexport function go() { return render(); }\n";
const IMPL: (&str, &str) = ("ts/impl.ts", "export function render() { return 1; }\n");
const NAMESAKE: (&str, &str) = ("ts/other.ts", "export function render() { return 2; }\n");

fn imports_of(provider: &dyn LanguageProvider, path: &str, source: &str) -> Vec<RawImport> {
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap()
        .imports
}

/// `export * from` binds no local name: one `*` import with no alias.
/// `export * as ns from` keeps its namespace alias and is not captured twice.
#[test]
fn test_parse_file_export_star_emits_unaliased_star_import() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    let source = "export * from './impl';\nexport * as ns from './other';\n";
    for (provider, path) in [
        (&ts as &dyn LanguageProvider, "index.ts"),
        (&js as &dyn LanguageProvider, "index.js"),
    ] {
        let shapes: Vec<_> = imports_of(provider, path, source)
            .into_iter()
            .map(|i| (i.source, i.imported_name, i.alias))
            .collect();
        assert_eq!(
            shapes,
            [
                ("./impl".to_string(), "*".to_string(), None),
                (
                    "./other".to_string(),
                    "*".to_string(),
                    Some("ns".to_string())
                ),
            ],
            "{path}"
        );
    }
}

#[test]
fn test_ts_reexport_star_ambiguous_name_binds_import_tier() {
    let graph = graph(&[
        ("ts/index.ts", "export * from './impl';\n"),
        IMPL,
        NAMESAKE,
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(&graph, "go", &[("ts/impl.ts", "", "render", import_tier())]);
}

#[test]
fn test_ts_reexport_star_unique_name_binds_import_tier() {
    let graph = graph(&[
        ("ts/index.ts", "export * from './impl';\n"),
        IMPL,
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(&graph, "go", &[("ts/impl.ts", "", "render", import_tier())]);
}

/// A named re-export, an import re-exported by a local `export { }`, and a
/// namespace import of the barrel all bind the re-exported declaration.
#[test]
fn test_ts_reexport_named_ambiguous_name_binds_import_tier() {
    for (barrel, app) in [
        ("export { render } from './impl';\n", APP),
        (
            "import { render } from './impl';\nexport { render };\n",
            APP,
        ),
        (
            "export * from './impl';\n",
            "import * as ui from './index';\nexport function go() { return ui.render(); }\n",
        ),
    ] {
        let graph = graph(&[("ts/index.ts", barrel), IMPL, NAMESAKE, ("ts/app.ts", app)]);
        assert_call_targets(&graph, "go", &[("ts/impl.ts", "", "render", import_tier())]);
    }
}

/// `export { draw as render }`: no file declares `render`, so only the alias
/// reaches `draw`, which is itself ambiguous to the global tier.
#[test]
fn test_ts_reexport_aliased_name_binds_declared_name() {
    let graph = graph(&[
        ("ts/index.ts", "export { draw as render } from './impl';\n"),
        ("ts/impl.ts", "export function draw() { return 1; }\n"),
        ("ts/other.ts", "export function draw() { return 2; }\n"),
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(&graph, "go", &[("ts/impl.ts", "", "draw", import_tier())]);
}

/// An explicit re-export shadows a star export of the same name, as in ES
/// modules.
#[test]
fn test_ts_reexport_explicit_shadows_star_binds_explicit_module() {
    let graph = graph(&[
        (
            "ts/index.ts",
            "export * from './other';\nexport { render } from './impl';\n",
        ),
        IMPL,
        NAMESAKE,
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(&graph, "go", &[("ts/impl.ts", "", "render", import_tier())]);
}

/// Two star modules declare `render`: ES modules leave the name unexported,
/// so the import tier does not guess and the call stays unresolved (no edge,
/// as on ecp 0.14.8).
#[test]
fn test_ts_reexport_two_star_sources_leave_name_unresolved() {
    let graph = graph(&[
        (
            "ts/index.ts",
            "export * from './impl';\nexport * from './other';\n",
        ),
        IMPL,
        NAMESAKE,
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

/// One hop: `index` → `mid` → `impl` is two. The second hop is not
/// followed: no edge when the name is ambiguous, the global tier when it is
/// unique.
#[test]
fn test_ts_reexport_two_hop_chain_keeps_global_tier() {
    let chain = [
        ("ts/index.ts", "export * from './mid';\n"),
        ("ts/mid.ts", "export * from './impl';\n"),
        IMPL,
        ("ts/app.ts", APP),
    ];
    let mut ambiguous = chain.to_vec();
    ambiguous.push(NAMESAKE);
    assert_call_targets(&graph(&ambiguous), "go", &[]);
    assert_call_targets(
        &graph(&chain),
        "go",
        &[("ts/impl.ts", "", "render", global_tier())],
    );
}

/// `export { render } from './impl'` where `impl` lacks `render`: no
/// import-tier edge, and the explicit binding hides no star fallback; the
/// global tier keeps its previous answer (ecp 0.14.8 alike).
#[test]
fn test_ts_reexport_name_missing_in_source_keeps_global_tier() {
    let graph = graph(&[
        ("ts/index.ts", "export { render } from './impl';\n"),
        ("ts/impl.ts", "export function tint() { return 1; }\n"),
        NAMESAKE,
        ("ts/app.ts", APP),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("ts/other.ts", "", "render", global_tier())],
    );
}

/// `a` star-exports `b` and `b` star-exports `a`: resolution ends, with no
/// edge, as no module declares `render`.
#[test]
fn test_ts_reexport_circular_star_terminates_without_edge() {
    let graph = graph(&[
        ("ts/a.ts", "export * from './b';\n"),
        ("ts/b.ts", "export * from './a';\n"),
        (
            "ts/app.ts",
            "import { render } from './a';\nexport function go() { return render(); }\n",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_js_reexport_star_ambiguous_name_binds_import_tier() {
    let graph = graph(&[
        ("js/index.js", "export * from './impl';\n"),
        ("js/impl.js", "export function render() { return 1; }\n"),
        ("js/other.js", "export function render() { return 2; }\n"),
        (
            "js/app.js",
            "import { render } from './index';\nexport function go() { return render(); }\n",
        ),
    ]);
    assert_call_targets(&graph, "go", &[("js/impl.js", "", "render", import_tier())]);
}

/// CommonJS `module.exports = require('./impl')` is no captured import, so
/// nothing is followed: no edge when ambiguous, the global tier when unique.
#[test]
fn test_js_commonjs_module_exports_require_resolves_as_before() {
    let barrel = [
        ("js/index.js", "module.exports = require('./impl');\n"),
        (
            "js/impl.js",
            "function render() { return 1; }\nmodule.exports = { render };\n",
        ),
        (
            "js/app.js",
            "const { render } = require('./index');\nfunction go() { return render(); }\n",
        ),
    ];
    let mut ambiguous = barrel.to_vec();
    ambiguous.push(("js/other.js", "function render() { return 2; }\n"));
    assert_call_targets(&graph(&ambiguous), "go", &[]);
    assert_call_targets(
        &graph(&barrel),
        "go",
        &[("js/impl.js", "", "render", global_tier())],
    );
}

/// Sibling language, same shape: a Dart barrel (`export 'impl.dart';`)
/// resolves as on ecp 0.14.8: no edge when ambiguous, the global tier when
/// unique.
#[test]
fn test_dart_barrel_export_resolves_as_before() {
    let dart = DartProvider::new().unwrap();
    let barrel = [
        ("lib/api.dart", "export 'impl.dart';\n"),
        ("lib/impl.dart", "void stamp() {}\n"),
        (
            "lib/app.dart",
            "import 'api.dart';\nvoid go() { stamp(); }\n",
        ),
    ];
    let mut ambiguous = barrel.to_vec();
    ambiguous.push(("lib/other.dart", "void stamp() {}\n"));
    assert_call_targets(&graph_of(&dart, &ambiguous), "go", &[]);
    assert_call_targets(
        &graph_of(&dart, &barrel),
        "go",
        &[("lib/impl.dart", "", "stamp", global_tier())],
    );
}
