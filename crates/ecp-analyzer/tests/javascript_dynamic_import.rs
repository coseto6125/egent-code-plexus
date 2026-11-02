//! FU-2026-11-01-c07a40e90e36 (T1): a literal dynamic module load gets an
//! `Imports` edge, and a call through the name that holds the module binds in
//! that module (0.95).
//!
//! Every fixture has a decoy: `b.js` declares `run` and `execute` as well,
//! so the unique-name fallback (0.7) cannot pick the right target and only
//! the import can. A name binds only when its `const` is a direct statement
//! of a function or program body and nothing else declares or writes the name
//! in that scope; a use textually before the declaration never binds. Every
//! other shape keeps today's result, and its test asserts that no call gets
//! the import tier. The `Imports` edge does not depend on the shape.
mod instantiation_calls_support;

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::imports_edges_of;
use std::path::Path;

const TARGET: &str = "export function run() {}\n";
const DECOY: &str = "export function run() {}\nexport function execute() {}\n";

fn import_tier() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn graph(app: &str) -> ZeroCopyGraph {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, source) in [("a.js", TARGET), ("b.js", DECOY), ("app.js", app)] {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap_or_else(|e| panic!("parse {path}: {e}")),
        );
    }
    builder.build()
}

fn raw_imports(source: &str) -> Vec<RawImport> {
    JavaScriptProvider::new()
        .expect("JavaScriptProvider::new")
        .parse_file(Path::new("app.js"), source.as_bytes())
        .expect("parse app.js")
        .imports
}

/// `(target file, target name, confidence)` of every `Calls` edge.
fn calls(graph: &ZeroCopyGraph) -> Vec<(String, String, f32)> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            (
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
                target.name.resolve(pool).to_string(),
                e.confidence,
            )
        })
        .collect()
}

fn imports_file(graph: &ZeroCopyGraph, file: &str) -> bool {
    imports_edges_of(graph, "app.js")
        .iter()
        .any(|(target, ..)| target == file)
}

fn assert_binds_target(app: &str) {
    let g = graph(app);
    assert_eq!(
        calls(&g),
        [("a.js".to_string(), "run".to_string(), import_tier())],
        "{app}"
    );
    assert!(imports_file(&g, "a.js"), "Imports edge to a.js: {app}");
}

fn assert_no_import_tier_call(app: &str) {
    let g = graph(app);
    let got = calls(&g);
    assert!(
        got.iter()
            .all(|(_, _, confidence)| *confidence < import_tier()),
        "no call may take the import tier: {app}; got {got:?}"
    );
}

#[test]
fn test_dynamic_import_namespace_call_binds_in_loaded_module() {
    assert_binds_target("export async function go() { const m = await import('./a'); m.run(); }\n");
}

#[test]
fn test_dynamic_import_destructured_alias_binds_the_export_not_the_local_name() {
    assert_binds_target(
        "export async function go() { const { run: execute } = await import('./a'); execute(); }\n",
    );
}

#[test]
fn test_dynamic_import_destructured_shorthand_binds_in_loaded_module() {
    assert_binds_target(
        "export async function go() { const { run } = await import('./a'); run(); }\n",
    );
}

#[test]
fn test_dynamic_import_call_in_closure_after_declaration_binds() {
    assert_binds_target(
        "export async function go() {\n  const m = await import('./a');\n  [1].forEach(() => m.run());\n}\n",
    );
}

#[test]
fn test_dynamic_import_top_level_await_binds_in_program_scope() {
    assert_binds_target("const m = await import('./a');\nexport function go() { m.run(); }\n");
}

#[test]
fn test_dynamic_import_function_scoped_require_binds() {
    assert_binds_target("export function go() { const m = require('./a'); m.run(); }\n");
}

#[test]
fn test_dynamic_import_two_scopes_bind_the_same_name_to_their_own_module() {
    let g = graph(
        "export async function one() { const m = await import('./a'); m.run(); }\n\
         export async function two() { const m = await import('./b'); m.run(); }\n",
    );
    let mut got = calls(&g);
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        got,
        [
            ("a.js".to_string(), "run".to_string(), import_tier()),
            ("b.js".to_string(), "run".to_string(), import_tier()),
        ]
    );
}

#[test]
fn test_dynamic_import_block_redeclaration_after_use_never_binds() {
    let app = "export async function f() {\n  const m = await import('./a');\n  { m.run(); const m = await import('./b'); }\n}\n";
    assert_no_import_tier_call(app);
    let g = graph(app);
    assert!(imports_file(&g, "a.js") && imports_file(&g, "b.js"));
}

#[test]
fn test_dynamic_import_use_before_declaration_in_closure_never_binds() {
    let app = "export async function f() {\n  function g() { return m.run(); }\n  const m = await import('./a');\n  return g();\n}\n";
    assert_no_import_tier_call(app);
    assert!(imports_file(&graph(app), "a.js"));
}

#[test]
fn test_dynamic_import_call_before_declaration_never_binds() {
    let app = "export async function go() {\n  m.run();\n  const m = await import('./a');\n}\n";
    assert_no_import_tier_call(app);
    assert!(imports_file(&graph(app), "a.js"));
}

#[test]
fn test_dynamic_import_unsupported_binding_shapes_never_bind() {
    for app in [
        "export async function go() { let m = await import('./a'); m.run(); }\n",
        "export async function go() { var m = await import('./a'); m.run(); }\n",
        "export async function go(x) { if (x) { const m = await import('./a'); m.run(); } }\n",
        "export async function go() { const m = await import('./a'); function h() { const m = { run() {} }; m.run(); } m.run(); }\n",
        "export async function go() { const m = await import('./a'); [m] = [1]; m.run(); }\n",
        "export async function go() { const m = await import('./a'); ({ m } = {}); m.run(); }\n",
        "export async function go() { const m = await import('./a'); function h(m) { m.run(); } m.run(); }\n",
        "export async function go() { const m = await import('./a'); try {} catch (m) { m.run(); } }\n",
        "export async function go() { const m = await import('./a'), n = 1; m.run(); n; }\n",
        "export async function go() { const { run: execute } = await import('./a'); function k(execute) { execute(); } execute(); }\n",
        "export async function go() { const { default: run } = await import('./a'); run(); }\n",
        "export async function go() { const { run = 1 } = await import('./a'); run(); }\n",
        "export async function go() { const m = import('./a'); m.run(); }\n",
        "export async function go() { import('./a').then((m) => m.run()); }\n",
    ] {
        assert_no_import_tier_call(app);
        assert!(imports_file(&graph(app), "a.js"), "Imports edge: {app}");
    }
}

#[test]
fn test_dynamic_import_require_that_is_not_the_loader_never_binds() {
    for app in [
        "export function go(require) { require('./a').run(); }\n",
        "export function go() { const require = (s) => ({ run() {} }); const m = require('./a'); m.run(); }\n",
        "export function go() { const m = require('./a'); m.run(); }\nfunction other(require) {}\n",
    ] {
        assert_no_import_tier_call(app);
        assert!(!imports_file(&graph(app), "a.js"), "no Imports edge: {app}");
    }
}

#[test]
fn test_dynamic_import_every_literal_form_emits_an_imports_edge() {
    for app in [
        "export async function go() { await import('./a'); }\n",
        "export async function go() { await import(`./a`); }\n",
        "export async function go() { await import('./a', { with: { type: 'json' } }); }\n",
        "export const p = import('./a');\n",
    ] {
        let g = graph(app);
        assert!(imports_file(&g, "a.js"), "Imports edge: {app}");
        assert_no_import_tier_call(app);
    }
}

#[test]
fn test_dynamic_import_nonliteral_and_empty_specifiers_emit_nothing() {
    for source in [
        "export async function go(p) { await import(p); }\n",
        "export async function go(p) { await import(`./${p}`); }\n",
        "export async function go() { await import(''); await import('  '); }\n",
        "export async function go() { await import('./a\\u0062'); }\n",
        "export const x = 1;\n",
    ] {
        assert!(raw_imports(source).is_empty(), "{source}");
    }
}

#[test]
fn test_dynamic_import_external_package_gets_no_project_edge() {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, source) in [
        ("lodash.js", "export function run() {}\n"),
        (
            "app.js",
            "export async function go() { const m = await import('lodash'); m.run(); }\n",
        ),
    ] {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .expect("parse"),
        );
    }
    let g = builder.build();
    assert!(!imports_file(&g, "lodash.js"));
}

#[test]
fn test_dynamic_import_aliases_follow_first_occurrence_of_each_specifier() {
    let imports = raw_imports(
        "export async function go() {\n  await import('./b');\n  await import('./a');\n  await import('./b');\n}\n",
    );
    let got: Vec<_> = imports
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
        got,
        [
            ("./b", "*", Some("@import0")),
            ("./a", "*", Some("@import1")),
        ]
    );
}
