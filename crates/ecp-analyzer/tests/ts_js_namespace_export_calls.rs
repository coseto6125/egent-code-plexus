//! An ES module exposes only what it exports. `import * as ns from './a';
//! ns.f()` and `import { f } from './a'` reach the declaration `a` exports as
//! `f`, never a module-private or nested namesake (FU-2026-10-04-14079e7e2d8b).
//! A module with no ES export at all (CommonJS, a script) keeps the by-name
//! lookup, and other languages keep theirs.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use std::path::Path;

/// `(callee, target file, 1-based line, confidence in percent)`.
type Call = (String, String, u32, u32);

const TS_JS: [&str; 2] = ["ts", "js"];

fn call(callee: &str, file: &str, line: u32, confidence: u32) -> Call {
    (callee.to_string(), file.to_string(), line, confidence)
}

/// Every Calls edge from `caller` after a full build of `files`, written to
/// disk so that relative imports and the Rust module tree resolve. Sorted.
fn calls_from(files: &[(&str, &str)], caller: &str) -> Vec<Call> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let js = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let py = PythonProvider::new().expect("PythonProvider::new");
    let rs = RustProvider::new().expect("RustProvider::new");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        let provider: &dyn LanguageProvider =
            match Path::new(rel).extension().and_then(|ext| ext.to_str()) {
                Some("ts") => &ts,
                Some("js") => &js,
                Some("py") => &py,
                Some("rs") => &rs,
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
    let mut out: Vec<Call> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            (
                target.name.resolve(pool).to_string(),
                graph.files[target.file_idx as usize]
                    .path
                    .resolve(pool)
                    .to_string(),
                target.span.0 + 1,
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `use` calls `ns.f()` on `module_src`, imported from `./a`, for TS and JS.
fn namespace_calls(ext: &str, module_src: &str) -> Vec<Call> {
    let module = format!("a.{ext}");
    let importer = format!("b.{ext}");
    calls_from(
        &[
            (&module, module_src),
            (
                &importer,
                "import * as ns from './a';\nfunction use() { ns.f(); }\n",
            ),
        ],
        "use",
    )
}

/// The FU repro. `ns.f` is `g` at run time and the private `f` (line 1) is
/// out of reach. Before the fix the import tier bound the private `f` at
/// 0.95. Binding `g` needs the local export aliases on record, which the
/// parse output does not carry yet; until then the contract is "no edge to
/// the private `f`", and this expectation becomes `g` (line 2) with them.
#[test]
fn test_resolve_call_namespace_member_shadowed_by_export_alias_binds_no_private_symbol() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function f() {}\nfunction g() {}\nexport { g as f };\n"
            ),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// The named-import twin of the FU repro. A second private `f` in an
/// unrelated module makes the global tier ambiguous, so the assertion
/// isolates the import tier: before the fix it bound `a`'s private `f`.
#[test]
fn test_resolve_call_named_import_shadowed_by_export_alias_binds_no_private_symbol() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let unrelated = format!("c.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "function f() {}\nfunction g() {}\nexport { g as f };\n"
                    ),
                    (&unrelated, "function f() {}\n"),
                    (
                        &importer,
                        "import { f } from './a';\nfunction use() { f(); }\n"
                    ),
                ],
                "use",
            ),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// `a` exports something, but not `f`. The import tier binds nothing, the
/// qualifier tier treats `ns` as the module and finds no export `f`, and a
/// qualified callee never reaches the global tier: the call is unresolved.
#[test]
fn test_resolve_call_namespace_member_private_only_falls_back_to_no_edge() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(ext, "function f() {}\nexport function other() {}\n"),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// A module with no ES export is a script or a CommonJS module: export
/// status says nothing about it, so the by-name lookup stays.
#[test]
fn test_resolve_call_namespace_member_without_es_exports_keeps_binding() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(ext, "function f() {}\n"),
            vec![call("f", &format!("a.{ext}"), 1, 95)],
            "{ext}"
        );
    }
}

/// `export { f }` exports the top-level `f` (line 5). Before the fix the
/// lookup took the first `f` in the file, the one nested in `outer`.
#[test]
fn test_resolve_call_namespace_member_unaliased_export_clause_binds_declaration() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function outer() {\n  function f() {}\n  f();\n}\nfunction f() {}\nexport { f };\n"
            ),
            vec![call("f", &format!("a.{ext}"), 5, 95)],
            "{ext}"
        );
    }
}

/// `export function f` (line 5) beats a same-named function nested in
/// `outer` (line 2), which came first in the file and won before the fix.
#[test]
fn test_resolve_call_namespace_member_exported_function_beats_nested_namesake() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function outer() {\n  function f() {}\n  f();\n}\nexport function f() {}\n"
            ),
            vec![call("f", &format!("a.{ext}"), 5, 95)],
            "{ext}"
        );
    }
}

/// Named imports of a declaration exported through a local clause keep
/// binding: the module is an ES module, and the clause marks `f` exported.
#[test]
fn test_resolve_call_named_import_of_clause_export_keeps_binding() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "function f() {}\nexport { f };\nexport function other() {}\n"
                    ),
                    (
                        &importer,
                        "import { f } from './a';\nfunction use() { f(); }\n"
                    ),
                ],
                "use",
            ),
            vec![call("f", &module, 1, 95)],
            "{ext}"
        );
    }
}

/// `new ns.Widget()` reaches a class exported through a local clause.
#[test]
fn test_resolve_call_namespace_construction_of_clause_exported_class_keeps_binding() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "class Widget {}\nexport { Widget };\nexport function other() {}\n"
                    ),
                    (
                        &importer,
                        "import * as ns from './a';\nfunction use() { return new ns.Widget(); }\n"
                    ),
                ],
                "use",
            ),
            vec![call("Widget", &module, 1, 95)],
            "{ext}"
        );
    }
}

/// A default import binds by the local name it is written with, which the
/// resolver matches against the declaration name. `export default h` must
/// therefore count `h` as exported, or `export function k` would make the
/// module an ES module that hides `h`.
#[test]
fn test_resolve_call_default_import_of_exported_identifier_keeps_binding() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "function h() {}\nexport default h;\nexport function k() {}\n"
                    ),
                    (&importer, "import h from './a';\nfunction use() { h(); }\n"),
                ],
                "use",
            ),
            vec![call("h", &module, 1, 95)],
            "{ext}"
        );
    }
}

/// `ns.default` names the default export. No declaration is named
/// `default`, so the call stays unresolved, as before the fix. Binding `h`
/// needs the same export-alias record as `export { g as f }`.
#[test]
fn test_resolve_call_namespace_default_member_stays_unresolved() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "function h() {}\nexport default h;\nexport function k() {}\n"
                    ),
                    (
                        &importer,
                        "import * as ns from './a';\nfunction use() { ns.default(); }\n"
                    ),
                ],
                "use",
            ),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// CommonJS modules export through assignments, which carry no export
/// status: `module.exports = { f }` and `exports.f = f` keep binding `f`.
#[test]
fn test_resolve_call_commonjs_exports_keep_binding() {
    for module_src in [
        "function f() {}\nmodule.exports = { f };\n",
        "function f() {}\nexports.f = f;\n",
    ] {
        assert_eq!(
            calls_from(
                &[
                    ("a.js", module_src),
                    (
                        "b.js",
                        "const ns = require('./a');\nfunction use() { ns.f(); }\n"
                    ),
                ],
                "use",
            ),
            vec![call("f", "a.js", 1, 95)],
            "{module_src}"
        );
    }
}

/// A barrel re-exports `impl` with `export *` and declares a private
/// `inner` of its own, which `export *` does not shadow: `ns.inner` is
/// `impl`'s. Before the fix the barrel's private `inner` was bound. Following
/// `export *` belongs to FU-2026-10-05-0ee319339782, so this accepts no edge
/// or `impl`'s `inner`, and nothing else.
#[test]
fn test_resolve_call_namespace_member_through_reexport_never_binds_barrel_private() {
    for ext in TS_JS {
        let implementation = format!("impl.{ext}");
        let barrel = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let calls = calls_from(
            &[
                (&implementation, "export function inner() {}\n"),
                (
                    &barrel,
                    "export * from './impl';\nfunction inner() {}\nexport function local() {}\n",
                ),
                (
                    &importer,
                    "import * as ns from './a';\nfunction use() { ns.inner(); }\n",
                ),
            ],
            "use",
        );
        assert!(
            calls.iter().all(|(callee, file, line, _)| callee == "inner"
                && *file == implementation
                && *line == 1),
            "{ext}: {calls:?}"
        );
    }
}

/// Python has no export statement; `_helper` is not exported by naming
/// convention, yet `import helpers; helpers._helper()` binds it. Export
/// status is a TS/JS rule only.
#[test]
fn test_resolve_call_python_module_import_binds_unexported_member() {
    assert_eq!(
        calls_from(
            &[
                ("helpers.py", "def _helper():\n    pass\n"),
                ("other.py", "def _helper():\n    pass\n"),
                (
                    "app.py",
                    "import helpers\n\ndef go():\n    helpers._helper()\n"
                ),
            ],
            "go",
        ),
        vec![call("_helper", "helpers.py", 1, 95)]
    );
}

/// Rust reaches through the named-import tier too: a child module imports
/// its parent's private `helper` with `use super::helper`.
#[test]
fn test_resolve_call_rust_use_of_private_parent_item_keeps_binding() {
    assert_eq!(
        calls_from(
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"x\"\nversion = \"0.0.0\"\n"
                ),
                ("src/lib.rs", "mod a;\n"),
                ("src/a/mod.rs", "mod b;\nfn helper() {}\n"),
                (
                    "src/a/b.rs",
                    "use super::helper;\npub fn go() { helper(); }\n"
                ),
            ],
            "go",
        ),
        vec![call("helper", "src/a/mod.rs", 2, 95)]
    );
}
