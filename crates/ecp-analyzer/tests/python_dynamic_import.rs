//! FU-2026-11-01-c07a40e90e36 (T1): a literal `importlib.import_module("<lit>")`
//! gets an `Imports` edge, and a call through the name that holds the module
//! binds in that module (0.95).
//!
//! Every fixture has a decoy: `b.py` declares `run` and `execute` as well, so
//! the unique-name fallback (0.7) cannot pick the right target and only the
//! import can. A name binds only when `m = <load>` is a direct statement of a
//! function or module body and nothing else declares or writes `m` in that
//! scope's subtree; a use textually before the assignment never binds. The
//! loader counts only when one plain `import importlib` (or `from importlib
//! import import_module`) binds its name and nothing else does. Every other
//! shape keeps today's result, and its test asserts that no call gets the
//! import tier. The `Imports` edge does not depend on the binding shape.
use crate::instantiation_calls_support;

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawImport;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::imports_edges_of;
use std::path::Path;

const TARGET: &str = "def run():\n    pass\n";
const DECOY: &str = "def run():\n    pass\n\n\ndef execute():\n    pass\n";
const MODULE_HEADER: &str = "import importlib\n\n\n";
const FUNCTION_HEADER: &str = "from importlib import import_module\n\n\n";

fn import_tier() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn graph_of(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    let mut builder = GraphBuilder::new();
    for (path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap_or_else(|e| panic!("parse {path}: {e}")),
        );
    }
    builder.build()
}

fn graph(app: &str) -> ZeroCopyGraph {
    graph_of(&[("a.py", TARGET), ("b.py", DECOY), ("app.py", app)])
}

fn raw_imports(source: &str) -> Vec<RawImport> {
    PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new("app.py"), source.as_bytes())
        .expect("parse app.py")
        .imports
}

/// The `(source, alias)` of every synthetic `@import<N>` import.
fn dynamic_imports(source: &str) -> Vec<(String, String)> {
    raw_imports(source)
        .into_iter()
        .filter_map(|i| {
            let alias = i.alias.filter(|a| a.starts_with("@import"))?;
            Some((i.source, alias))
        })
        .collect()
}

/// How many `python-dynamic-import` blind spots `source` reports.
fn dynamic_import_blind_spots(source: &str) -> usize {
    PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new("app.py"), source.as_bytes())
        .expect("parse app.py")
        .blind_spots
        .iter()
        .filter(|b| b.kind == "python-dynamic-import")
        .count()
}

/// `(target file, confidence)` of every `Calls` edge from `caller`.
fn calls_from(graph: &ZeroCopyGraph, caller: &str) -> Vec<(String, f32)> {
    let pool = graph.string_pool.as_slice();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            (
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
                e.confidence,
            )
        })
        .collect()
}

/// `(target file, target name, confidence)` of every `Calls` edge that leaves
/// `app.py`. A call into the importer's own file is the same-file tier (1.0),
/// not an import.
fn foreign_calls(graph: &ZeroCopyGraph) -> Vec<(String, String, f32)> {
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
        .filter(|(file, ..)| file != "app.py")
        .collect()
}

fn imports_file(graph: &ZeroCopyGraph, file: &str) -> bool {
    imports_edges_of(graph, "app.py")
        .iter()
        .any(|(target, ..)| target == file)
}

fn assert_binds_target(app: &str) {
    let g = graph(app);
    assert_eq!(
        foreign_calls(&g),
        [("a.py".to_string(), "run".to_string(), import_tier())],
        "{app}"
    );
    assert!(imports_file(&g, "a.py"), "Imports edge to a.py: {app}");
}

fn assert_no_import_tier_call(app: &str) {
    let got = foreign_calls(&graph(app));
    assert!(
        got.iter()
            .all(|(_, _, confidence)| *confidence < import_tier()),
        "no call may take the import tier: {app}; got {got:?}"
    );
}

#[test]
fn test_dynamic_import_namespace_call_binds_in_loaded_module() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"
    ));
}

#[test]
fn test_dynamic_import_from_import_form_binds_in_loaded_module() {
    assert_binds_target(&format!(
        "{FUNCTION_HEADER}def go():\n    m = import_module(\"a\")\n    m.run()\n"
    ));
}

#[test]
fn test_dynamic_import_single_quoted_literal_binds() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}def go():\n    m = importlib.import_module('a')\n    m.run()\n"
    ));
}

#[test]
fn test_dynamic_import_submodule_import_of_importlib_still_proves_the_loader() {
    assert_binds_target(
        "import importlib.util\n\n\ndef go():\n    m = importlib.import_module(\"a\")\n    m.run()\n",
    );
}

#[test]
fn test_dynamic_import_function_local_loader_import_binds() {
    assert_binds_target(
        "def go():\n    import importlib\n    m = importlib.import_module(\"a\")\n    m.run()\n",
    );
}

#[test]
fn test_dynamic_import_module_scope_name_binds_in_function_after_it() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}m = importlib.import_module(\"a\")\n\n\ndef go():\n    m.run()\n"
    ));
}

#[test]
fn test_dynamic_import_call_in_nested_function_after_assignment_binds() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n\n    def inner():\n        return m.run()\n\n    return inner\n"
    ));
}

#[test]
fn test_dynamic_import_call_in_comprehension_after_assignment_binds() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n    return [m.run() for _ in range(2)]\n"
    ));
}

#[test]
fn test_dynamic_import_async_function_scope_binds() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}async def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"
    ));
}

#[test]
fn test_dynamic_import_method_scope_binds() {
    assert_binds_target(&format!(
        "{MODULE_HEADER}class K:\n    def go(self):\n        m = importlib.import_module(\"a\")\n        m.run()\n"
    ));
}

/// The binding ends with its scope: a sibling function after it that calls
/// through the same name keeps today's result.
#[test]
fn test_dynamic_import_sibling_function_after_the_scope_never_binds() {
    let g = graph(&format!(
        "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n\n\ndef later():\n    m.run()\n"
    ));
    assert_eq!(calls_from(&g, "go"), [("a.py".to_string(), import_tier())]);
    let leaked: Vec<_> = calls_from(&g, "later")
        .into_iter()
        .filter(|(file, confidence)| file == "a.py" && *confidence >= import_tier())
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
}

#[test]
fn test_dynamic_import_two_scopes_bind_the_same_name_to_their_own_module() {
    let g = graph(&format!(
        "{MODULE_HEADER}def one():\n    m = importlib.import_module(\"a\")\n    m.run()\n\n\ndef two():\n    m = importlib.import_module(\"b\")\n    m.run()\n"
    ));
    let mut got = foreign_calls(&g);
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        got,
        [
            ("a.py".to_string(), "run".to_string(), import_tier()),
            ("b.py".to_string(), "run".to_string(), import_tier()),
        ]
    );
}

#[test]
fn test_dynamic_import_dotted_module_binds_in_the_submodule() {
    let g = graph_of(&[
        ("pkg/__init__.py", TARGET),
        ("pkg/sub.py", TARGET),
        (
            "app.py",
            format!(
                "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"pkg.sub\")\n    m.run()\n"
            )
            .as_str(),
        ),
    ]);
    assert_eq!(
        foreign_calls(&g),
        [("pkg/sub.py".to_string(), "run".to_string(), import_tier())]
    );
    assert!(imports_file(&g, "pkg/sub.py"));
}

#[test]
fn test_dynamic_import_call_before_assignment_never_binds() {
    let app =
        format!("{MODULE_HEADER}def go():\n    m.run()\n    m = importlib.import_module(\"a\")\n");
    assert_no_import_tier_call(&app);
    assert!(imports_file(&graph(&app), "a.py"));
}

#[test]
fn test_dynamic_import_use_before_assignment_in_closure_never_binds() {
    let app = format!(
        "{MODULE_HEADER}def go():\n    def g():\n        return m.run()\n\n    m = importlib.import_module(\"a\")\n    return g()\n"
    );
    assert_no_import_tier_call(&app);
    assert!(imports_file(&graph(&app), "a.py"));
}

#[test]
fn test_dynamic_import_module_scope_function_defined_before_assignment_never_binds() {
    let app = format!(
        "{MODULE_HEADER}def go():\n    return m.run()\n\n\nm = importlib.import_module(\"a\")\n"
    );
    assert_no_import_tier_call(&app);
    assert!(imports_file(&graph(&app), "a.py"));
}

#[test]
fn test_dynamic_import_rebinding_in_a_nested_block_never_binds() {
    for app in [
        format!(
            "{FUNCTION_HEADER}def go():\n    m = import_module(\"a\")\n    if True:\n        m = import_module(\"b\")\n    m.run()\n"
        ),
        format!(
            "{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n    if True:\n        m = importlib.import_module(\"b\")\n    m.run()\n"
        ),
    ] {
        assert_no_import_tier_call(&app);
        let g = graph(&app);
        assert!(imports_file(&g, "a.py") && imports_file(&g, "b.py"), "{app}");
    }
}

#[test]
fn test_dynamic_import_unsupported_binding_shapes_never_bind() {
    let body = |code: &str| format!("{MODULE_HEADER}{code}");
    for app in [
        body("def go(x):\n    if x:\n        m = importlib.import_module(\"a\")\n        m.run()\n"),
        body("def go():\n    try:\n        m = importlib.import_module(\"a\")\n    except ImportError:\n        return\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    m = make()\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    m += 1\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n\n    def h():\n        m = make()\n        m.run()\n\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n\n    def h(m):\n        m.run()\n\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    f = lambda m: m.run()\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    for m in range(2):\n        pass\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    with open(\"x\") as m:\n        pass\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    try:\n        pass\n    except ValueError as m:\n        pass\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    import os as m\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    del m\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    del (m, go)\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    (m := make())\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    [m for m in range(2)]\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n    m, other = make()\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n\n    def m():\n        pass\n\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n\n    class m:\n        pass\n\n    m.run()\n"),
        body("def go():\n    global m\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        body("def go():\n    m = importlib.import_module(\"a\")\n\n    def h():\n        nonlocal m\n        m = make()\n\n    m.run()\n"),
        body("def go():\n    m: object = importlib.import_module(\"a\")\n    m.run()\n"),
        body("def go():\n    m, n = importlib.import_module(\"a\"), 1\n    m.run()\n"),
        body("def go():\n    m = n = importlib.import_module(\"a\")\n    m.run()\n"),
        body("def go():\n    m = lambda: importlib.import_module(\"a\")\n    m().run()\n"),
        body("def go():\n    importlib.import_module(\"a\").run()\n"),
        body("class K:\n    m = importlib.import_module(\"a\")\n\n    def go(self):\n        m.run()\n"),
        body("m = importlib.import_module(\"a\")\n\n\ndef go(m):\n    m.run()\n"),
        body("m = importlib.import_module(\"a\")\n\n\ndef go():\n    m = make()\n    m.run()\n"),
        body("if True:\n    m = importlib.import_module(\"a\")\n\n\ndef go():\n    m.run()\n"),
    ] {
        assert_no_import_tier_call(&app);
        assert!(imports_file(&graph(&app), "a.py"), "Imports edge: {app}");
    }
}

#[test]
fn test_dynamic_import_loader_that_is_not_importlib_never_binds() {
    for app in [
        "def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n".to_string(),
        "def go():\n    m = import_module(\"a\")\n    m.run()\n".to_string(),
        "import importlib as il\n\n\ndef go():\n    m = il.import_module(\"a\")\n    m.run()\n".to_string(),
        "from importlib import import_module as load\n\n\ndef go():\n    m = load(\"a\")\n    m.run()\n".to_string(),
        "from other import import_module\n\n\ndef go():\n    m = import_module(\"a\")\n    m.run()\n".to_string(),
        "from . import import_module\n\n\ndef go():\n    m = import_module(\"a\")\n    m.run()\n".to_string(),
        "import import_module\n\n\ndef go():\n    m = import_module(\"a\")\n    m.run()\n".to_string(),
        format!("{MODULE_HEADER}importlib = fake()\n\n\ndef go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go(importlib):\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        format!("{FUNCTION_HEADER}def import_module(name):\n    return None\n\n\ndef go():\n    m = import_module(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}from os.path import *\n\n\ndef go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}import importlib\n\n\ndef go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        "class K:\n    import importlib\n\n    def go(self):\n        m = importlib.import_module(\"a\")\n        m.run()\n".to_string(),
        "class K:\n    import importlib\n    m = importlib.import_module(\"a\")\n".to_string(),
        "def other():\n    import importlib\n\n\ndef go():\n    m = importlib.import_module(\"a\")\n    m.run()\n".to_string(),
        "def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n\n\nimport importlib\n".to_string(),
    ] {
        assert_no_import_tier_call(&app);
        assert!(
            dynamic_imports(&app).is_empty(),
            "no synthetic import: {app}"
        );
        assert!(!imports_file(&graph(&app), "a.py"), "no Imports edge: {app}");
    }
}

#[test]
fn test_dynamic_import_function_local_loader_does_not_prove_a_call_before_it() {
    let app =
        "def go():\n    m = importlib.import_module(\"a\")\n    import importlib\n    m.run()\n";
    assert_no_import_tier_call(app);
    assert!(dynamic_imports(app).is_empty());
}

#[test]
fn test_dynamic_import_dunder_import_and_unsupported_arguments_emit_nothing() {
    for app in [
        format!("{MODULE_HEADER}def go():\n    m = __import__(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.__import__(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\".a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\".a\", \"pkg\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\", package=\"pkg\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\", \"pkg\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(name=\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go(p):\n    m = importlib.import_module(p)\n    m.run()\n"),
        format!("{MODULE_HEADER}def go(p):\n    m = importlib.import_module(f\"{{p}}\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go(p):\n    m = importlib.import_module(\"a\" + p)\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\" \"b\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(*[\"a\"])\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module()\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"  \")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a b\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a..b\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a.\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(b\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(r\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\\x62\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(None)\n    m.run()\n"),
    ] {
        assert_no_import_tier_call(&app);
        assert!(
            dynamic_imports(&app).is_empty(),
            "no synthetic import: {app}"
        );
        assert!(!imports_file(&graph(&app), "a.py"), "no Imports edge: {app}");
    }
}

#[test]
fn test_dynamic_import_dunder_import_of_dotted_name_binds_neither_package_nor_submodule() {
    let g = graph_of(&[
        ("pkg/__init__.py", TARGET),
        ("pkg/sub.py", TARGET),
        (
            "app.py",
            "def go():\n    m = __import__(\"pkg.sub\")\n    m.run()\n",
        ),
    ]);
    assert!(foreign_calls(&g)
        .iter()
        .all(|(_, _, confidence)| *confidence < import_tier()));
    assert!(!imports_file(&g, "pkg/sub.py") && !imports_file(&g, "pkg/__init__.py"));
}

#[test]
fn test_dynamic_import_relative_name_with_package_binds_neither_module() {
    let g = graph_of(&[
        ("pkg/__init__.py", ""),
        ("pkg/x.py", TARGET),
        ("pkg/y.py", TARGET),
        (
            "app.py",
            format!(
                "{FUNCTION_HEADER}def go():\n    m = import_module(\".x\", \"pkg\")\n    m.run()\n"
            )
            .as_str(),
        ),
    ]);
    assert!(foreign_calls(&g)
        .iter()
        .all(|(_, _, confidence)| *confidence < import_tier()));
    assert!(!imports_file(&g, "pkg/x.py") && !imports_file(&g, "pkg/y.py"));
}

#[test]
fn test_dynamic_import_every_literal_form_emits_an_imports_edge() {
    for app in [
        format!("{MODULE_HEADER}def go():\n    importlib.import_module(\"a\")\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\"a\")\n"),
        format!("{MODULE_HEADER}importlib.import_module(\"a\")\n"),
        format!("{FUNCTION_HEADER}def go():\n    return import_module(\"a\")\n"),
        format!("{MODULE_HEADER}x = importlib.import_module(\"a\")\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\"\"\"a\"\"\")\n"),
    ] {
        let g = graph(&app);
        assert!(imports_file(&g, "a.py"), "Imports edge: {app}");
        assert_no_import_tier_call(&app);
    }
}

/// A load the collector resolved names its module statically, so it reports
/// no runtime-value blind spot; every load it refused keeps one.
#[test]
fn test_dynamic_import_resolved_load_reports_no_runtime_value_blind_spot() {
    for app in [
        format!("{MODULE_HEADER}def go():\n    m = importlib.import_module(\"a\")\n    m.run()\n"),
        format!("{MODULE_HEADER}def go():\n    importlib.import_module(\"a\").run()\n"),
        format!("{MODULE_HEADER}x = importlib.import_module(\"pkg.sub\")\n"),
    ] {
        assert_eq!(dynamic_import_blind_spots(&app), 0, "{app}");
    }
}

#[test]
fn test_dynamic_import_refused_load_keeps_its_blind_spot() {
    for app in [
        format!("{MODULE_HEADER}def go(name):\n    return importlib.import_module(name)\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\".a\")\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\".a\", \"pkg\")\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\"a\", package=\"pkg\")\n"),
        format!("{MODULE_HEADER}def go():\n    return importlib.import_module(\"\")\n"),
        format!("{MODULE_HEADER}def go(importlib):\n    return importlib.import_module(\"a\")\n"),
        "def go():\n    return importlib.import_module(\"a\")\n".to_string(),
        format!("{MODULE_HEADER}from os.path import *\n\n\ndef go():\n    return importlib.import_module(\"a\")\n"),
    ] {
        assert_eq!(dynamic_import_blind_spots(&app), 1, "{app}");
    }
}

#[test]
fn test_dynamic_import_blind_spot_follows_each_load_not_the_file() {
    let app = format!(
        "{MODULE_HEADER}def go(name):\n    a = importlib.import_module(\"a\")\n    b = importlib.import_module(name)\n    return a, b\n"
    );
    let spots: Vec<u32> = PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new("app.py"), app.as_bytes())
        .expect("parse app.py")
        .blind_spots
        .iter()
        .filter(|b| b.kind == "python-dynamic-import")
        .map(|b| b.span.0)
        .collect();
    assert_eq!(spots, [5], "only the variable load on line 6 (row 5)");
}

#[test]
fn test_dynamic_import_empty_and_loadless_sources_emit_nothing() {
    for source in [
        "",
        "x = 1\n",
        "import importlib\n",
        "from importlib import import_module\n",
        "import importlib\nimportlib.reload(importlib)\n",
        "import importlib\n\n\ndef go(p):\n    return importlib.import_module(p)\n",
    ] {
        assert!(dynamic_imports(source).is_empty(), "{source}");
    }
}

/// A bare module name resolves as the static `import` of it does: the dynamic
/// load adds no resolution rule of its own. Both sides are `true` today: for
/// Python a top-level name is a module of the project root, so the edge to
/// `lodash.py` is the static import's own result. FU-2026-11-02-7859ceda0be1
/// (a bare package specifier binding a same-stem project file) is a TS/JS
/// resolver defect; its no-edge pin lives in the TS and JS files.
#[test]
fn test_dynamic_import_bare_name_resolves_like_static_import() {
    let edge_to_namesake = |app: &str| {
        imports_file(
            &graph_of(&[("lodash.py", TARGET), ("app.py", app)]),
            "lodash.py",
        )
    };
    assert_eq!(
        edge_to_namesake(&format!(
            "{MODULE_HEADER}def go():\n    importlib.import_module(\"lodash\")\n"
        )),
        edge_to_namesake("import lodash\n\n\ndef go():\n    lodash.run()\n"),
    );
}

#[test]
fn test_dynamic_import_dotted_name_is_a_path_source_like_a_static_import() {
    // `import pkg.sub` also records the root binding `pkg`.
    let static_source = raw_imports("import pkg.sub\n")
        .into_iter()
        .map(|i| i.source)
        .find(|source| source != "pkg")
        .expect("static import of pkg.sub");
    assert_eq!(
        dynamic_imports(&format!(
            "{MODULE_HEADER}def go():\n    importlib.import_module(\"pkg.sub\")\n"
        )),
        [(static_source, "@import0".to_string())]
    );
}

#[test]
fn test_dynamic_import_aliases_follow_first_occurrence_of_each_specifier() {
    let imports = raw_imports(&format!(
        "{MODULE_HEADER}def go():\n    importlib.import_module(\"b\")\n    importlib.import_module(\"a\")\n    importlib.import_module(\"b\")\n"
    ));
    let got: Vec<_> = imports
        .iter()
        .filter(|i| i.alias.as_deref().is_some_and(|a| a.starts_with("@import")))
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
        [("b", "*", Some("@import0")), ("a", "*", Some("@import1"))]
    );
}

#[test]
fn test_dynamic_import_static_imports_keep_their_own_aliases() {
    let imports = raw_imports(&format!(
        "{MODULE_HEADER}def go():\n    importlib.import_module(\"a\")\n"
    ));
    assert!(
        imports
            .iter()
            .any(|i| i.source == "importlib" && i.alias.as_deref() == Some("importlib")),
        "{imports:?}"
    );
}
