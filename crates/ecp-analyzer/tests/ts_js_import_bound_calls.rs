//! A TS/JS call through a module binding resolves in the module the binding
//! names. Each case has a second file defining the same function name, so a
//! by-name guess is ambiguous and only the import can pick the target.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use std::path::Path;

/// Target files of every Calls edge from `go` to `callee`, sorted.
fn go_targets(files: &[(&str, &str)], callee: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let js = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        let provider: &dyn LanguageProvider = match Path::new(rel).extension() {
            Some(ext) if ext == "ts" => &ts,
            Some(ext) if ext == "js" => &js,
            _ => continue,
        };
        builder.add_graph(
            provider
                .parse_file(Path::new(rel), src.as_bytes())
                .expect("parse_file"),
        );
    }
    let graph = builder.build();
    let pool = graph.string_pool.as_slice();
    let mut out: Vec<String> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == "go")
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            let file = graph.nodes[e.target as usize].file_idx as usize;
            graph.files[file].path.resolve(pool).to_string()
        })
        .collect();
    out.sort();
    out
}

/// `import * as ns from "./a"; ns.f()` (FU-2026-10-04-4e78262aabe9).
#[test]
fn test_namespace_import_call_resolves_in_the_imported_module() {
    for (ext, app) in [
        (
            "ts",
            "import * as ns from \"./a\";\nexport function go() { ns.f(); }\n",
        ),
        (
            "js",
            "import * as ns from \"./a\";\nexport function go() { ns.f(); }\n",
        ),
    ] {
        let a = format!("a.{ext}");
        let b = format!("b.{ext}");
        let main = format!("app.{ext}");
        let files = [
            (a.as_str(), "export function f() {}\n"),
            (b.as_str(), "export function f() {}\n"),
            (main.as_str(), app),
        ];
        assert_eq!(go_targets(&files, "f"), vec![a.clone()], "{ext}");
    }
}

/// CommonJS bindings: `const m = require("./a"); m.f()` and
/// `const { f } = require("./a"); f()` (FU-2026-10-04-1fdc6ad9dc95).
#[test]
fn test_require_binding_call_resolves_in_the_required_module() {
    for app in [
        "const m = require(\"./a\");\nfunction go() { m.f(); }\n",
        "const { f } = require(\"./a\");\nfunction go() { f(); }\n",
        "const { f: g } = require(\"./a\");\nfunction go() { g(); }\n",
        "const f = require(\"./a\").f;\nfunction go() { f(); }\n",
    ] {
        let files = [
            ("a.js", "function f() {}\nmodule.exports = { f };\n"),
            ("b.js", "function f() {}\nmodule.exports = { f };\n"),
            ("app.js", app),
        ];
        assert_eq!(go_targets(&files, "f"), vec!["a.js".to_string()], "{app}");
    }
}

/// A require of a package (not a relative path) names no project file, so it
/// must not bind a project function of the same name.
#[test]
fn test_require_of_a_package_never_binds_a_project_function() {
    let files = [
        ("a.js", "function f() {}\nmodule.exports = { f };\n"),
        (
            "app.js",
            "const { f } = require(\"lodash\");\nfunction go() { f(); }\n",
        ),
    ];
    assert_eq!(go_targets(&files, "f"), Vec::<String>::new());
}
