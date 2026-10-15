//! FU-2026-10-05-0ee319339782: a TS / JS call through a barrel module
//! (`export * from './impl'`, `export { x } from './impl'`) binds through
//! the import tier (0.95), following the re-export one hop. Before the fix
//! these calls kept the global tier: 0.7 for a unique name, no edge for an
//! ambiguous one, and the parsers emitted nothing for `export * from`.
//!
//! Only what the barrel exports is followed (a plain import is not; codex
//! c3), the default-export barrel forms bind the default declaration (R1
//! F4), and a consumer keeps its File edge to the barrel (codex c1).
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
use ecp_core::analyzer::types::{BindingKind, RawImport};
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::{assert_call_targets, graph_of, imports_edges_of};
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

/// `a` star-exports `b` and `b` star-exports `a`. One hop through the cycle
/// reaches `b`'s own `render` (`other.ts` makes the name ambiguous to the
/// global tier), and a name no module declares ends with no edge instead
/// of following the cycle.
#[test]
fn test_ts_reexport_circular_star_binds_one_hop_and_terminates() {
    let graph = graph(&[
        ("ts/a.ts", "export * from './b';\n"),
        (
            "ts/b.ts",
            "export * from './a';\nexport function render() { return 1; }\n",
        ),
        NAMESAKE,
        (
            "ts/app.ts",
            "import { render, missing } from './a';\nexport function go() { return render(); }\nexport function stop() { return missing(); }\n",
        ),
    ]);
    assert_call_targets(&graph, "go", &[("ts/b.ts", "", "render", import_tier())]);
    assert_call_targets(&graph, "stop", &[]);
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

/// `{ext}/<name>.{ext}`, the layout of the TS and JS fixtures below.
fn at(ext: &str, name: &str) -> String {
    format!("{ext}/{name}.{ext}")
}

/// codex c1 / R1 F1: a consumer's import through a barrel binds the
/// re-exported declaration and keeps its File → barrel File edge (0.9),
/// which ecp 0.14.8 emitted and the edit hook's `importers()` reads. Before
/// this fix only the symbol edge was left, so the barrel had no importer.
/// An import of the module that declares the name gets no File edge.
#[test]
fn test_emit_edges_named_import_through_barrel_keeps_barrel_file_edge() {
    for ext in ["ts", "js"] {
        for barrel in [
            "export * from './impl';\n",
            "export { render } from './impl';\n",
            "import { render } from './impl';\nexport { render };\n",
        ] {
            let (index, implementation) = (at(ext, "index"), at(ext, "impl"));
            let (app, direct) = (at(ext, "app"), at(ext, "direct"));
            let graph = graph(&[
                (index.as_str(), barrel),
                (implementation.as_str(), IMPL.1),
                (app.as_str(), APP),
                (
                    direct.as_str(),
                    "import { render } from './impl';\nexport function go2() { return render(); }\n",
                ),
            ]);
            assert_eq!(
                imports_edges_of(&graph, &format!("app.{ext}")),
                [
                    (implementation.clone(), "render".to_string(), "Function", 95),
                    (index.clone(), format!("index.{ext}"), "File", 90),
                ],
                "{ext}: {barrel}"
            );
            assert_eq!(
                imports_edges_of(&graph, &format!("direct.{ext}")),
                [(implementation.clone(), "render".to_string(), "Function", 95)],
                "{ext}: a direct import"
            );
        }
    }
}

/// codex c3 / R1 F5: a barrel's plain `import { render } from './impl'`
/// exports nothing. `api.render()` through `import * as api` and a named
/// import of `render` reach no export of the barrel, so neither binds
/// `impl`'s `render` (no edge, as on ecp 0.14.8); before this fix both bound
/// it at 0.95. The barrel's own call still binds its import.
#[test]
fn test_ts_reexport_plain_import_in_barrel_is_not_followed() {
    for ext in ["ts", "js"] {
        let [index, implementation, other, app, app2] =
            ["index", "impl", "other", "app", "app2"].map(|name| at(ext, name));
        let graph = graph(&[
            (
                index.as_str(),
                "import { render } from './impl';\nexport function local() { return render(); }\n",
            ),
            (implementation.as_str(), IMPL.1),
            (other.as_str(), NAMESAKE.1),
            (
                app.as_str(),
                "import * as api from './index';\nexport function go() { return api.render(); }\n",
            ),
            (
                app2.as_str(),
                "import { render } from './index';\nexport function go2() { return render(); }\n",
            ),
        ]);
        assert_call_targets(&graph, "go", &[]);
        assert_call_targets(&graph, "go2", &[]);
        assert_call_targets(
            &graph,
            "local",
            &[(implementation.as_str(), "", "render", import_tier())],
        );
    }
}

/// R1 F5: the barrel imports `render` from `legacy` for its own use and
/// star-exports `impl`. The plain import does not shadow the star export,
/// so a consumer binds `impl`'s `render`; before this fix it bound
/// `legacy`'s at 0.95.
#[test]
fn test_ts_reexport_plain_import_does_not_shadow_star_binds_star_module() {
    for ext in ["ts", "js"] {
        let [index, implementation, legacy, app] =
            ["index", "impl", "legacy", "app"].map(|name| at(ext, name));
        let graph = graph(&[
            (
                index.as_str(),
                "import { render } from './legacy';\nexport * from './impl';\nexport function draw() { return render(); }\n",
            ),
            (implementation.as_str(), IMPL.1),
            (legacy.as_str(), "export function render() { return 3; }\n"),
            (app.as_str(), APP),
        ]);
        assert_call_targets(
            &graph,
            "go",
            &[(implementation.as_str(), "", "render", import_tier())],
        );
        assert_call_targets(
            &graph,
            "draw",
            &[(legacy.as_str(), "", "render", import_tier())],
        );
    }
}

/// R1 F4 / codex c7: the default-export barrel forms bind `impl`'s default
/// export `Button` (`other`'s namesake makes the global tier ambiguous):
/// `export { default as Button } from` (the JS query did not capture it),
/// a default import the barrel exports (`import Button from; export {
/// Button }`, also renamed, which the re-export gate must reach), and a
/// default import through `export { default } from`. Before this fix none
/// bound in JS, and only the first in TS.
#[test]
fn test_ts_reexport_default_export_barrel_forms_bind_default_declaration() {
    for ext in ["ts", "js"] {
        let [index, implementation, other, app_path] =
            ["index", "impl", "other", "app"].map(|name| at(ext, name));
        for (barrel, app) in [
            (
                "export { default as Button } from './impl';\n",
                "import { Button } from './index';\nexport function go() { return Button(); }\n",
            ),
            (
                "import Button from './impl';\nexport { Button };\n",
                "import { Button } from './index';\nexport function go() { return Button(); }\n",
            ),
            (
                "import Btn from './impl';\nexport { Btn };\n",
                "import { Btn } from './index';\nexport function go() { return Btn(); }\n",
            ),
            (
                "export { default } from './impl';\n",
                "import Button from './index';\nexport function go() { return Button(); }\n",
            ),
        ] {
            let graph = graph(&[
                (index.as_str(), barrel),
                (
                    implementation.as_str(),
                    "export default function Button() { return 1; }\n",
                ),
                (
                    other.as_str(),
                    "export default function Button() { return 2; }\n",
                ),
                (app_path.as_str(), app),
            ]);
            assert_call_targets(
                &graph,
                "go",
                &[(implementation.as_str(), "", "Button", import_tier())],
            );
        }
    }
}

/// The parsers mark what the barrel exports: a re-export (`export { x }
/// from`, `export { default as X } from`) and an import its own `export { }`
/// clause exports under its local name are [`BindingKind::EsReexport`],
/// [`BindingKind::EsDefaultReexport`] for a default import. An import
/// exported only under another name (`z as w`) and a plain import stay
/// unmarked.
#[test]
fn test_parse_file_reexport_marks_exported_bindings() {
    let ts = TypeScriptProvider::new().unwrap();
    let js = JavaScriptProvider::new().unwrap();
    let source = "import d from './a';\nimport { e, z } from './a';\nimport { g as h } from './a';\nimport k from './a';\nimport { p } from './a';\nimport q from './a';\nexport { default as X } from './m';\nexport { y } from './m';\nexport { q } from './a';\nexport { e, h, k, z as w };\n";
    for (provider, path) in [
        (&ts as &dyn LanguageProvider, "index.ts"),
        (&js as &dyn LanguageProvider, "index.js"),
    ] {
        let imports = imports_of(provider, path, source);
        let kind_of = |name: &str, alias: Option<&str>| {
            let found: Vec<_> = imports
                .iter()
                .filter(|import| import.imported_name == name && import.alias.as_deref() == alias)
                .map(|import| import.binding_kind)
                .collect();
            assert_eq!(found.len(), 1, "{path}: {name} in {imports:?}");
            found[0]
        };
        assert_eq!(kind_of("d", None), Some(BindingKind::EsDefault), "{path}");
        assert_eq!(kind_of("e", None), Some(BindingKind::EsReexport), "{path}");
        assert_eq!(
            kind_of("g", Some("h")),
            Some(BindingKind::EsReexport),
            "{path}"
        );
        assert_eq!(
            kind_of("k", None),
            Some(BindingKind::EsDefaultReexport),
            "{path}"
        );
        assert_eq!(
            kind_of("default", Some("X")),
            Some(BindingKind::EsReexport),
            "{path}"
        );
        assert_eq!(kind_of("y", None), Some(BindingKind::EsReexport), "{path}");
        assert_eq!(kind_of("z", None), None, "{path}: exported only as `w`");
        assert_eq!(kind_of("p", None), None, "{path}: a plain import");
        // `import q` and `export { q } from` emit the same shape; each keeps
        // the kind of its own statement.
        let q: Vec<_> = imports
            .iter()
            .filter(|import| import.imported_name == "q" && import.alias.is_none())
            .map(|import| import.binding_kind)
            .collect();
        assert!(
            q.len() == 2
                && q.contains(&Some(BindingKind::EsDefault))
                && q.contains(&Some(BindingKind::EsReexport)),
            "{path}: {q:?}"
        );
    }
}
