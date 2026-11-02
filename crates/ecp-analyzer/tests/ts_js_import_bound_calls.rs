//! A TS/JS call through a module binding resolves in the module the binding
//! names. Each case has a second file defining the same function name, so a
//! by-name guess is ambiguous and only the import can pick the target.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use std::path::Path;

fn import_tier() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

/// Target files of every Calls edge from `go` to `callee`, sorted.
fn go_targets(files: &[(&str, &str)], callee: &str) -> Vec<String> {
    targets_from(files, "go", callee, 0.0)
}

/// Target files of every Calls edge from `caller` to `callee` whose
/// confidence is at least `min_confidence`, sorted.
fn targets_from(
    files: &[(&str, &str)],
    caller: &str,
    callee: &str,
    min_confidence: f32,
) -> Vec<String> {
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
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| e.confidence >= min_confidence)
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

#[test]
fn test_namespace_import_missing_member_keeps_no_fallback() {
    for ext in ["ts", "js"] {
        let module = format!("a.{ext}");
        let other = format!("b.{ext}");
        let app = format!("app.{ext}");
        assert!(
            go_targets(
                &[
                    (&module, "export const value = 1;\n"),
                    (&other, "export function f() {}\n"),
                    (
                        &app,
                        "import * as ns from './a';\nexport function go() { ns.f(); }\n"
                    ),
                ],
                "f"
            )
            .is_empty(),
            "{ext}"
        );
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

/// A `const` `require` that is a direct statement of a function body binds
/// inside that function only (T1, FU-2026-11-01-c07a40e90e36): `later`, a
/// sibling after it, gets no edge into the required module.
#[test]
fn test_require_inside_a_function_binds_in_that_function() {
    let files = [
        ("a.js", "function f() {}\nmodule.exports = { f };\n"),
        ("b.js", "function f() {}\nmodule.exports = { f };\n"),
        (
            "app.js",
            "function go() {\n  const { f } = require(\"./a\");\n  f();\n}\nfunction later() { f(); }\n",
        ),
    ];
    assert_eq!(go_targets(&files, "f"), vec!["a.js".to_string()]);
    assert!(targets_from(&files, "later", "f", import_tier()).is_empty());
}

/// Two functions that each require `f` from a different module. A file-wide
/// import would send `go`'s call to `./b`, the first require in the file;
/// each function-scoped require binds its own scope, so `go` reaches `./a`.
#[test]
fn test_function_scoped_requires_of_two_modules_bind_their_own_scope() {
    let files = [
        ("a.js", "function f() {}\nmodule.exports = { f };\n"),
        ("b.js", "function f() {}\nmodule.exports = { f };\n"),
        (
            "app.js",
            "function first() { const { f } = require(\"./b\"); f(); }\n\
             function go() { const { f } = require(\"./a\"); f(); }\n\
             function later() { f(); }\n",
        ),
    ];
    assert_eq!(go_targets(&files, "f"), vec!["a.js".to_string()]);
    assert_eq!(
        targets_from(&files, "first", "f", 0.0),
        vec!["b.js".to_string()]
    );
    assert!(targets_from(&files, "later", "f", import_tier()).is_empty());
}

/// `require` shadowed by a parameter is no module loader: the declarator
/// form records no import, so no edge goes into the file it seems to name.
#[test]
fn test_shadowed_require_call_emits_no_edge() {
    let files = [
        ("a.js", "function f() {}\nmodule.exports = { f };\n"),
        ("b.js", "function f() {}\nmodule.exports = { f };\n"),
        (
            "app.js",
            "function go(require) {\n  const { f } = require(\"./a\");\n  f();\n}\n",
        ),
    ];
    assert_eq!(go_targets(&files, "f"), Vec::<String>::new());
}

/// The same module imported by `import` and by `require` records two imports
/// but yields one Calls edge per call site.
#[test]
fn test_es_import_and_require_of_one_module_emit_one_edge() {
    let files = [
        ("a.js", "function f() {}\nmodule.exports = { f };\n"),
        ("b.js", "function f() {}\nmodule.exports = { f };\n"),
        (
            "app.js",
            "import { f } from \"./a\";\nconst m = require(\"./a\");\nfunction go() { f(); }\n",
        ),
    ];
    assert_eq!(go_targets(&files, "f"), vec!["a.js".to_string()]);
}

/// A require of a non-literal or whitespace-only specifier records no import
/// and does not crash.
#[test]
fn test_require_with_a_blank_or_computed_specifier_binds_nothing() {
    let src = "const { f } = require(\"  \");\nconst m = require(name);\nfunction go() { f(); }\n";
    let js = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    for (path, provider) in [
        ("app.js", &js as &dyn LanguageProvider),
        ("app.ts", &ts as &dyn LanguageProvider),
    ] {
        let graph = provider
            .parse_file(Path::new(path), src.as_bytes())
            .expect("parse_file");
        assert!(graph.imports.is_empty(), "{path}: {:?}", graph.imports);
    }
}
