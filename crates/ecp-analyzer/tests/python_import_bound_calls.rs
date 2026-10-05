//! Explicit imports must choose their module even when another module has the same name.
mod instantiation_calls_support;

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use instantiation_calls_support::{calls_from, graph_of};
use std::path::Path;

fn assert_python_call(files: &[(&str, &str)], caller: &str, target: &str, name: &str) {
    let graph = graph_of(&PythonProvider::new().unwrap(), files);
    let hits = calls_from(&graph, caller);
    assert!(
        hits.iter()
            .any(|hit| hit.file == target && hit.name == name),
        "{hits:?}"
    );
}

#[test]
fn test_import_root_bindings_deduplicates_namespace() {
    let graph = PythonProvider::new()
        .unwrap()
        .parse_file(Path::new("app.py"), b"import a.b\nimport a.c\nimport a\n")
        .unwrap();
    assert_eq!(
        graph
            .imports
            .iter()
            .filter(|import| import.source == "a" && import.alias.as_deref() == Some("a"))
            .count(),
        1
    );
}

#[test]
fn test_import_relative_cache_keeps_caller_directories_separate() {
    let graph = graph_of(
        &PythonProvider::new().unwrap(),
        &[
            ("a/util.py", "def helper(): pass\n"),
            ("b/util.py", "def helper(): pass\n"),
            (
                "a/app.py",
                "from .util import helper\ndef first(): return helper()\n",
            ),
            (
                "b/app.py",
                "from .util import helper\ndef second(): return helper()\n",
            ),
        ],
    );
    assert_eq!(calls_from(&graph, "first")[0].file, "a/util.py");
    assert_eq!(calls_from(&graph, "second")[0].file, "b/util.py");
}

#[test]
fn test_import_assignment_after_call_keeps_module_binding() {
    assert_python_call(
        &[
            ("pkg/util.py", "def helper(): pass\n"),
            ("other.py", "def helper(): pass\n"),
            (
                "app.py",
                "import pkg.util as api\ndef go(value):\n    api.helper()\n    api = value\n",
            ),
        ],
        "go",
        "pkg/util.py",
        "helper",
    );
}

#[test]
fn test_import_conditional_missing_then_local_preserves_call() {
    assert_python_call(&[
        ("app/compat/__init__.py", "from .impl import dumps\n"),
        ("app/compat/impl.py", "def dumps(): pass\n"),
        ("app/main.py", "try:\n    from orjson import dumps\nexcept ImportError:\n    from app.compat import dumps\ndef save(): return dumps()\n"),
    ], "save", "app/compat/impl.py", "dumps");
}

#[test]
fn test_import_namespace_package_preserves_call() {
    assert_python_call(
        &[
            ("ns/mod.py", "def helper(): pass\n"),
            (
                "main.py",
                "import ns\nfrom ns import mod\ndef go(): return ns.mod.helper()\n",
            ),
        ],
        "go",
        "ns/mod.py",
        "helper",
    );
}

#[test]
fn test_import_namespace_sibling_preserves_call() {
    assert_python_call(
        &[
            ("pkg/a.py", ""),
            ("pkg/b.py", "def run(): pass\n"),
            ("main.py", "import pkg.a\ndef go(): return pkg.b.run()\n"),
        ],
        "go",
        "pkg/b.py",
        "run",
    );
}

#[test]
fn test_import_script_directory_preserves_call() {
    assert_python_call(
        &[
            ("tools/__init__.py", ""),
            ("tools/util.py", "def helper(): pass\n"),
            (
                "tools/run.py",
                "from util import helper\ndef go(): return helper()\n",
            ),
        ],
        "go",
        "tools/util.py",
        "helper",
    );
}

#[test]
fn test_import_backend_source_root_preserves_call() {
    assert_python_call(
        &[
            ("backend/__init__.py", ""),
            ("backend/app/__init__.py", ""),
            ("backend/app/services.py", "def helper(): pass\n"),
            (
                "backend/main.py",
                "from app.services import helper\ndef go(): return helper()\n",
            ),
        ],
        "go",
        "backend/app/services.py",
        "helper",
    );
}

#[test]
fn test_import_relative_repo_root_resolves_call() {
    assert_python_call(
        &[
            ("__init__.py", "def helper(): pass\n"),
            (
                "models/partner.py",
                "from .. import helper\ndef go(): return helper()\n",
            ),
        ],
        "go",
        "__init__.py",
        "helper",
    );
}

#[test]
fn test_import_parameter_shadow_preserves_call() {
    assert_python_call(
        &[
            ("worker.py", "def helper(): pass\n"),
            (
                "main.py",
                "if TYPE_CHECKING:\n    import external as api\ndef go(api): return api.helper()\n",
            ),
        ],
        "go",
        "worker.py",
        "helper",
    );
}

#[test]
fn test_import_assignment_shadow_preserves_call() {
    assert_python_call(&[
        ("worker.py", "def helper(): pass\n"),
        ("main.py", "import external as api\ndef go(value):\n    api = value\n    return api.helper()\n"),
    ], "go", "worker.py", "helper");
}

#[test]
fn test_import_package_precedes_module_resolves_package() {
    assert_python_call(
        &[
            ("pkg.py", "def helper(): pass\n"),
            ("pkg/__init__.py", "def helper(): pass\n"),
            (
                "main.py",
                "from pkg import helper\ndef go(): return helper()\n",
            ),
        ],
        "go",
        "pkg/__init__.py",
        "helper",
    );
}

#[test]
fn test_import_source_and_stub_resolves_source() {
    assert_python_call(
        &[
            ("src/lib/mod.py", "def helper(): pass\n"),
            ("src/lib/mod.pyi", "def helper(): ...\n"),
            (
                "main.py",
                "from lib.mod import helper\ndef go(): return helper()\n",
            ),
        ],
        "go",
        "src/lib/mod.py",
        "helper",
    );
}

#[test]
fn test_import_untyped_member_ignores_named_import() {
    assert_python_call(
        &[
            (
                "worker.py",
                "class Joiner:\n    def join(self, parts): pass\n",
            ),
            (
                "main.py",
                "from os.path import join\ndef go(sep, parts): return sep.join(parts)\n",
            ),
        ],
        "go",
        "worker.py",
        "join",
    );
}

#[test]
fn test_import_field_lookup_ignores_named_import() {
    use ecp_analyzer::resolution::{
        index::{ResolveTarget, SymbolTable},
        resolver::Resolver,
    };
    use ecp_core::{analyzer::types::RawImport, graph::NodeKind};
    let mut symbols = SymbolTable::new();
    symbols.register_node("worker.py", "value", 0, NodeKind::Property);
    let resolver = Resolver::new(&symbols);
    let imports = [RawImport {
        source: "external".into(),
        imported_name: "value".into(),
        alias: None,
        binding_kind: None,
    }];
    assert_eq!(
        resolver
            .resolve_symbol(
                Path::new("main.py"),
                "value",
                &imports,
                ResolveTarget::Field
            )
            .len(),
        1
    );
}

#[test]
fn test_import_module_alias_heritage_resolves_base_method() {
    assert_python_call(&[
        ("src/pkg/models.py", "class Base:\n    def save(self): pass\n"),
        ("src/app/x.py", "import pkg.models as models\nclass C(models.Base):\n    def save(self): return super().save()\n"),
    ], "save", "src/pkg/models.py", "save");
}

#[test]
fn test_import_multiline_attribute_resolves_call() {
    assert_python_call(
        &[
            ("pkg/util.py", "def helper(): pass\n"),
            (
                "main.py",
                "import pkg.util\ndef go():\n    return (pkg.util\n        .helper())\n",
            ),
        ],
        "go",
        "pkg/util.py",
        "helper",
    );
}

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
    assert_eq!(
        edge.confidence,
        ecp_analyzer::resolution::heuristics::ResolutionTier::ImportScoped.base_confidence(),
        "must use ImportScoped"
    );
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
fn test_import_package_reexport_falls_back_to_global_name() {
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
fn test_import_indexed_nested_package_falls_back_to_global_name() {
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
    assert_eq!(calls_from(&graph, "go")[0].file, "src/pkg/json/__init__.py");
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

/// `pkg` is an indexed directory, so the missing module `pkg.extra` may be
/// local: the call keeps the global tier instead of being suppressed. The
/// parser joins absolute sources with `/` (`pkg/extra`), so the first-segment
/// split on `/` and on `.` agree.
#[test]
fn test_import_dotted_missing_module_with_indexed_root_keeps_global() {
    let graph = graph_of(
        &PythonProvider::new().unwrap(),
        &[
            ("pkg/util.py", "def other():\n    pass\n"),
            ("lib/extra/x.py", "def helper():\n    pass\n"),
            (
                "app.py",
                "from pkg.extra import helper\n\ndef go():\n    return helper()\n",
            ),
        ],
    );
    let hits = calls_from(&graph, "go");
    assert!(
        hits.len() == 1
            && hits[0].file == "lib/extra/x.py"
            && hits[0].confidence
                == ecp_analyzer::resolution::heuristics::ResolutionTier::Global.base_confidence(),
        "{hits:?}"
    );
}
