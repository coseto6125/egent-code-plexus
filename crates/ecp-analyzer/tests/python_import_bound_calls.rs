//! Explicit imports must choose their module even when another module has the same name.
mod instantiation_calls_support;

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use instantiation_calls_support::{calls_from, graph_of};
use std::path::Path;

fn assert_import_target(import: &str, call: &str) {
    let provider = PythonProvider::new().unwrap();
    let app = format!("{import}\n\ndef go():\n    return {call}\n");
    let graph = graph_of(
        &provider,
        &[
            ("pkg/__init__.py", ""),
            ("pkg/util.py", "def helper():\n    return 1\n"),
            ("pkg/other.py", "def helper():\n    return 2\n"),
            ("pkg/app.py", &app),
        ],
    );
    let hits = calls_from(&graph, "go");
    assert_eq!(hits.len(), 1, "{import}: {hits:?}");
    assert_eq!(hits[0].file, "pkg/util.py");
    assert_eq!(hits[0].name, "helper");
    assert!(
        graph.edges.iter().any(|edge| {
            edge.rel_type == ecp_core::graph::RelType::Imports
                && graph.files[graph.nodes[edge.source as usize].file_idx as usize]
                    .path
                    .resolve(&graph.string_pool)
                    == "pkg/app.py"
                && graph.files[graph.nodes[edge.target as usize].file_idx as usize]
                    .path
                    .resolve(&graph.string_pool)
                    == "pkg/util.py"
        }),
        "{import}: missing Imports edge"
    );
    let edge = graph
        .edges
        .iter()
        .find(|edge| {
            graph.nodes[edge.source as usize]
                .name
                .resolve(&graph.string_pool)
                == "go"
                && edge.rel_type == ecp_core::graph::RelType::Calls
        })
        .unwrap();
    assert_eq!(edge.confidence, 0.95, "must use ImportScoped");
}

#[test]
fn test_import_absolute_duplicate_name_resolves_import() {
    assert_import_target("from pkg.util import helper", "helper()");
}

#[test]
fn test_import_relative_duplicate_name_resolves_import() {
    assert_import_target("from .util import helper", "helper()");
}

#[test]
fn test_import_alias_duplicate_name_resolves_import() {
    assert_import_target("from pkg.util import helper as h", "h()");
}

#[test]
fn test_import_module_duplicate_name_resolves_import() {
    assert_import_target("import pkg.util", "pkg.util.helper()");
}

#[test]
fn test_import_module_alias_duplicate_name_resolves_import() {
    assert_import_target("import pkg.util as u", "u.helper()");
}

#[test]
fn test_import_relative_alias_duplicate_name_resolves_import() {
    assert_import_target("from .util import helper as h", "h()");
}

#[test]
fn test_import_external_module_has_no_unrelated_call() {
    let provider = PythonProvider::new().unwrap();
    for (import, call) in [
        ("import external", "external.helper()"),
        ("import external as u", "u.helper()"),
        ("from external import helper", "helper()"),
    ] {
        let app = format!("{import}\n\ndef go():\n    return {call}\n");
        let graph = graph_of(
            &provider,
            &[
                ("pkg/util.py", "def other():\n    pass\n"),
                ("elsewhere/util.py", "def helper():\n    pass\n"),
                ("pkg/app.py", &app),
            ],
        );
        assert!(calls_from(&graph, "go").is_empty(), "{import}");
    }
}

#[test]
fn test_import_package_reexport_preserves_previous_call() {
    let provider = PythonProvider::new().unwrap();
    for (import, call) in [
        ("import pkg", "pkg.helper()"),
        ("import pkg.util", "pkg.helper()"),
        ("import pkg as p", "p.helper()"),
        ("import pkg", "pkg.util.helper()"),
        ("from pkg import helper", "helper()"),
    ] {
        let app = format!("{import}\n\ndef go():\n    return {call}\n");
        let graph = graph_of(
            &provider,
            &[
                ("src/pkg/__init__.py", "from .util import helper\n"),
                ("fixtures/pkg.py", "unused = 1\n"),
                ("src/pkg/util.py", "def helper():\n    pass\n"),
                ("tests/app.py", &app),
            ],
        );
        let hits = calls_from(&graph, "go");
        assert_eq!(hits.len(), 1, "{import}: {hits:?}");
        assert_eq!(hits[0].file, "src/pkg/util.py");
    }
}

#[test]
fn test_import_module_source_root_duplicate_name_resolves_import() {
    let provider = PythonProvider::new().unwrap();
    for app in [
        "import pkg.util\n\ndef go():\n    return pkg.util.helper()\n",
        "from pkg.util import helper\n\ndef go():\n    return helper()\n",
    ] {
        let graph = graph_of(
            &provider,
            &[
                ("src/pkg/util.py", "def helper():\n    pass\n"),
                ("src/other.py", "def helper():\n    pass\n"),
                ("tests/app.py", app),
            ],
        );
        let hits = calls_from(&graph, "go");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].file, "src/pkg/util.py");
    }
}

#[test]
fn test_import_object_member_keeps_existing_method_call() {
    let provider = PythonProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            (
                "pkg/util.py",
                "class Service:\n    def work(self):\n        pass\n\nservice = Service()\n",
            ),
            (
                "pkg/app.py",
                "from .util import service\n\ndef go():\n    return service.work()\n",
            ),
        ],
    );
    let hits = calls_from(&graph, "go");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].name, "work");
    assert_eq!(hits[0].file, "pkg/util.py");
}

#[test]
fn test_import_external_module_does_not_match_nested_package() {
    let provider = PythonProvider::new().unwrap();
    let graph = graph_of(
        &provider,
        &[
            ("src/pkg/__init__.py", ""),
            ("src/pkg/json/__init__.py", "def dumps():\n    pass\n"),
            (
                "src/pkg/app.py",
                "import json\n\ndef go():\n    return json.dumps({})\n",
            ),
        ],
    );
    assert!(calls_from(&graph, "go").is_empty());
}

#[test]
fn test_import_mixed_language_same_module_selects_python() {
    let provider = PythonProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    for (file, source) in [
        ("pkg/util.py", "def helper():\n    pass\n"),
        (
            "pkg/app.py",
            "from pkg.util import helper\n\ndef go():\n    return helper()\n",
        ),
    ] {
        builder.add_graph(
            provider
                .parse_file(Path::new(file), source.as_bytes())
                .unwrap(),
        );
    }
    builder.add_graph(
        TypeScriptProvider::new()
            .unwrap()
            .parse_file(Path::new("pkg/util.ts"), b"export function helper() {}\n")
            .unwrap(),
    );
    let hits = calls_from(&builder.build(), "go");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].file, "pkg/util.py");
}
