//! An ES module exposes only what it exports. `import * as ns from './a';
//! ns.f()` and `import { f } from './a'` reach the declaration `a` exports as
//! `f`, never a module-private or nested namesake (FU-2026-10-04-14079e7e2d8b).
//! An export under another name binds the declaration it names
//! (`export { g as f }`, `export default h`; FU-2026-10-15-1a48011441a4), and
//! the global tier never binds a declaration of the imported ES module that
//! the import tier found unexported (FU-2026-10-15-a79b39506bec). A module
//! with no ES export at all (CommonJS, a script) keeps the by-name lookup,
//! and other languages keep theirs.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::typescript::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{BindingKind, LocalGraph, RawExportAlias};
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
    calls_from_edited(files, caller, |_| {})
}

/// [`calls_from`], with `edit` applied to each parse output before the build.
fn calls_from_edited(
    files: &[(&str, &str)],
    caller: &str,
    edit: impl Fn(&mut LocalGraph),
) -> Vec<Call> {
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
        let mut local = provider
            .parse_file(Path::new(rel), src.as_bytes())
            .expect("parse_file");
        edit(&mut local);
        builder.add_graph(local);
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

/// `use` in `b` (`importer_src`) after a build of `a` (`module_src`) and `b`.
fn importer_calls(ext: &str, module_src: &str, importer_src: &str) -> Vec<Call> {
    let module = format!("a.{ext}");
    let importer = format!("b.{ext}");
    calls_from(&[(&module, module_src), (&importer, importer_src)], "use")
}

/// `use` calls `ns.<member>()` on `module_src`, imported from `./a`.
fn namespace_member_calls(ext: &str, module_src: &str, member: &str) -> Vec<Call> {
    importer_calls(
        ext,
        module_src,
        &format!("import * as ns from './a';\nfunction use() {{ ns.{member}(); }}\n"),
    )
}

/// `use` calls `ns.f()` on `module_src`, imported from `./a`, for TS and JS.
fn namespace_calls(ext: &str, module_src: &str) -> Vec<Call> {
    namespace_member_calls(ext, module_src, "f")
}

/// `use` calls `f()` through `import { f } from './a'`.
fn named_import_calls(ext: &str, module_src: &str, name: &str) -> Vec<Call> {
    importer_calls(
        ext,
        module_src,
        &format!("import {{ {name} }} from './a';\nfunction use() {{ {name}(); }}\n"),
    )
}

/// `use` calls `d()` through the default import `import d from './a'`.
fn default_import_calls(ext: &str, module_src: &str) -> Vec<Call> {
    importer_calls(
        ext,
        module_src,
        "import d from './a';\nfunction use() { d(); }\n",
    )
}

/// The FU repro. `ns.f` is `g` at run time and the private `f` (line 1) is
/// out of reach. The import tier first bound the private `f` at 0.95, then
/// no edge while the parse output carried no export aliases. With `f` → `g`
/// on record it binds `g` (line 2), as the earlier version of this test
/// said it would.
#[test]
fn test_resolve_call_namespace_member_shadowed_by_export_alias_binds_alias_target() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function f() {}\nfunction g() {}\nexport { g as f };\n"
            ),
            vec![call("g", &format!("a.{ext}"), 2, 95)],
            "{ext}"
        );
    }
}

/// The named-import twin of the FU repro. A second private `f` in an
/// unrelated module makes the global tier ambiguous, so the assertion
/// isolates the import tier: it first bound `a`'s private `f`, then no edge
/// while export aliases were not on record, and now `g` (line 2).
#[test]
fn test_resolve_call_named_import_shadowed_by_export_alias_binds_alias_target() {
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
            vec![call("g", &module, 2, 95)],
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

/// `ns.default` names the default export, `h` (line 1). No declaration is
/// named `default`, so the call stayed unresolved until the parse output
/// carried the export alias `default` → `h`; it now binds `h`.
#[test]
fn test_resolve_call_namespace_default_member_binds_default_export() {
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
            vec![call("h", &module, 1, 95)],
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
/// `impl`'s `inner` (line 1), bound by the import tier. Before the fix the
/// barrel's private `inner` was bound.
#[test]
fn test_resolve_call_namespace_member_through_reexport_binds_star_module_export() {
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
        assert_eq!(calls, vec![call("inner", &implementation, 1, 95)], "{ext}");
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

/// `export { g as default }` makes `g` (line 2) the default export, for a
/// namespace member and for a default import alike.
#[test]
fn test_resolve_call_export_alias_to_default_binds_target() {
    let module_src = "function f() {}\nfunction g() {}\nexport { g as default };\n";
    for ext in TS_JS {
        let expected = vec![call("g", &format!("a.{ext}"), 2, 95)];
        assert_eq!(
            namespace_member_calls(ext, module_src, "default"),
            expected,
            "{ext}: ns.default"
        );
        assert_eq!(default_import_calls(ext, module_src), expected, "{ext}: d");
    }
}

/// `import d from './a'` binds `a`'s default export `g` (line 2) under any
/// local name. Before the fix the import looked up a declaration named `d`.
#[test]
fn test_resolve_call_default_import_of_export_default_identifier_binds_declaration() {
    for ext in TS_JS {
        assert_eq!(
            default_import_calls(ext, "function f() {}\nfunction g() {}\nexport default g;\n"),
            vec![call("g", &format!("a.{ext}"), 2, 95)],
            "{ext}"
        );
    }
}

/// `export default function h() {}` (line 2) is the default export.
#[test]
fn test_resolve_call_default_import_of_default_function_binds_declaration() {
    let module_src = "function f() {}\nexport default function h() {}\n";
    for ext in TS_JS {
        let expected = vec![call("h", &format!("a.{ext}"), 2, 95)];
        assert_eq!(default_import_calls(ext, module_src), expected, "{ext}: d");
        assert_eq!(
            namespace_member_calls(ext, module_src, "default"),
            expected,
            "{ext}: ns.default"
        );
    }
}

/// A CommonJS module records no default export, so a default import keeps
/// the by-name lookup of its local name, as before the fix.
#[test]
fn test_resolve_call_default_import_of_commonjs_module_keeps_by_name_binding() {
    for ext in TS_JS {
        assert_eq!(
            importer_calls(
                ext,
                "function f() {}\nmodule.exports = f;\n",
                "import f from './a';\nfunction use() { f(); }\n",
            ),
            vec![call("f", &format!("a.{ext}"), 1, 95)],
            "{ext}"
        );
    }
}

/// R1 F2: `export default withAuth(handler)` binds no declaration, so a
/// default import keeps the by-name binding of its local name (`handler`,
/// line 2), as ecp 0.14.8 did. Before this fix the module's other export
/// made it an ES module, the import bound nothing, and the private
/// `handler` was hidden from the global tier. A module with no default
/// export at all still binds no default import (codex c6).
#[test]
fn test_resolve_call_default_import_of_expression_default_binds_local_name() {
    let importer = "import handler from './a';\nfunction use() { handler(); }\n";
    for ext in TS_JS {
        let module = format!("a.{ext}");
        for module_src in [
            "export const config = 1;\nfunction handler() {}\nexport default withAuth(handler);\n",
            "function x() {}\nfunction handler() {}\nexport default withAuth(handler);\n",
        ] {
            assert_eq!(
                importer_calls(ext, module_src, importer),
                vec![call("handler", &module, 2, 95)],
                "{ext}: {module_src}"
            );
        }
        assert_eq!(
            importer_calls(
                ext,
                "export const config = 1;\nfunction handler() {}\n",
                importer
            ),
            Vec::<Call>::new(),
            "{ext}: no default export"
        );
    }
}

/// codex C3: a module whose only export is `export default wrap(f)` is an
/// ES module, so `f` stays private: a named import or a namespace member
/// call binds no edge, and the global tier does not bind it either. Before
/// the fix the expression default alone built no alias table, the module
/// counted as a script, and both bound the private `f` (0.95).
#[test]
fn test_resolve_call_expression_default_only_module_keeps_private_name_hidden() {
    let module_src = "function f() {}\nexport default wrap(f);\n";
    for ext in TS_JS {
        assert_eq!(
            named_import_calls(ext, module_src, "f"),
            Vec::<Call>::new(),
            "{ext}: f"
        );
        assert_eq!(
            namespace_calls(ext, module_src),
            Vec::<Call>::new(),
            "{ext}: ns.f"
        );
    }
}

/// Q13: a class exported under another name (`export { Impl as Widget }`)
/// is the qualifier of `Widget.create()`: the qualifier tier finds `a`
/// through the module's exports (0.85). `c`'s namesake makes the global tier
/// ambiguous. Before the fix the qualifier was looked up by the name
/// `Widget`, which `a` does not declare, and the call bound nothing.
#[test]
fn test_resolve_call_aliased_class_export_qualifier_binds_qualifier_tier() {
    for ext in TS_JS {
        let (module, importer, other) =
            (format!("a.{ext}"), format!("b.{ext}"), format!("c.{ext}"));
        assert_eq!(
            calls_from(
                &[
                    (
                        &module,
                        "class Impl { static create() { return 1; } }\nexport { Impl as Widget };\n",
                    ),
                    (
                        &importer,
                        "import { Widget } from './a';\nfunction use() { Widget.create(); }\n",
                    ),
                    (
                        &other,
                        "export class Other { static create() { return 2; } }\n",
                    ),
                ],
                "use",
            ),
            vec![call("create", &module, 1, 85)],
            "{ext}"
        );
    }
}

/// `export { missing as f }` names no declaration of `a`: no edge through the
/// namespace or the named import, no crash, and the private `f` stays out of
/// reach of the global tier too. Before the fix `a` counted as no ES module
/// and both imports bound the private `f`.
#[test]
fn test_resolve_call_export_alias_to_undeclared_name_binds_no_edge() {
    let module_src = "function f() {}\nexport { missing as f };\n";
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(ext, module_src),
            Vec::<Call>::new(),
            "{ext}: ns.f"
        );
        assert_eq!(
            named_import_calls(ext, module_src, "f"),
            Vec::<Call>::new(),
            "{ext}: f"
        );
    }
}

/// Two export names for one declaration both bind it.
#[test]
fn test_resolve_call_two_aliases_of_one_declaration_bind_it() {
    let module_src = "function f() {}\nfunction g() {}\nexport { g as f, g as h };\n";
    for ext in TS_JS {
        let expected = vec![call("g", &format!("a.{ext}"), 2, 95)];
        assert_eq!(namespace_calls(ext, module_src), expected, "{ext}: ns.f");
        assert_eq!(
            namespace_member_calls(ext, module_src, "h"),
            expected,
            "{ext}: ns.h"
        );
    }
}

/// Each export name equals the other declaration's name: `f` is `g` and `g`
/// is `f`. Before the fix both bound their namesakes.
#[test]
fn test_resolve_call_swapped_export_aliases_bind_each_other() {
    let module_src = "function f() {}\nfunction g() {}\nexport { g as f, f as g };\n";
    for ext in TS_JS {
        let module = format!("a.{ext}");
        assert_eq!(
            namespace_calls(ext, module_src),
            vec![call("g", &module, 2, 95)],
            "{ext}: ns.f"
        );
        assert_eq!(
            namespace_member_calls(ext, module_src, "g"),
            vec![call("f", &module, 1, 95)],
            "{ext}: ns.g"
        );
    }
}

/// An unaliased export beside an alias keeps binding its declaration.
#[test]
fn test_resolve_call_unaliased_export_beside_alias_binds_declaration() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function f() {}\nfunction g() {}\nexport { f, g as h };\n"
            ),
            vec![call("f", &format!("a.{ext}"), 1, 95)],
            "{ext}"
        );
    }
}

/// `g` is exported only as `f`, so `import { g }` reaches nothing, and the
/// global tier does not bind the unique `g` either. Before the fix the
/// import tier bound `g` at 0.95.
#[test]
fn test_resolve_call_named_import_of_name_exported_only_as_alias_binds_no_edge() {
    for ext in TS_JS {
        assert_eq!(
            named_import_calls(
                ext,
                "function f() {}\nfunction g() {}\nexport { g as f };\n",
                "g"
            ),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// The same export clause listed twice binds as once.
#[test]
fn test_resolve_call_export_alias_listed_twice_binds_target() {
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(
                ext,
                "function f() {}\nfunction g() {}\nexport { g as f };\nexport { g as f };\n"
            ),
            vec![call("g", &format!("a.{ext}"), 2, 95)],
            "{ext}"
        );
    }
}

/// One export name for two declarations is an ES error: neither binds, and
/// the global tier does not bind the module's private `f` either.
#[test]
fn test_resolve_call_conflicting_export_aliases_bind_no_edge() {
    let module_src =
        "function f() {}\nfunction g() {}\nfunction h() {}\nexport { g as f, h as f };\n";
    for ext in TS_JS {
        assert_eq!(
            namespace_calls(ext, module_src),
            Vec::<Call>::new(),
            "{ext}: ns.f"
        );
        assert_eq!(
            named_import_calls(ext, module_src, "f"),
            Vec::<Call>::new(),
            "{ext}: f"
        );
    }
}

/// Blank entries in the alias record (no parser emits them) are skipped,
/// and an empty record reads as none: `f` then has no export, as `g` is
/// exported under no recorded name.
#[test]
fn test_resolve_call_blank_or_empty_export_alias_record_is_ignored() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let importer = format!("b.{ext}");
        let files = [
            (
                module.as_str(),
                "function f() {}\nfunction g() {}\nexport { g as f };\n",
            ),
            (
                importer.as_str(),
                "import * as ns from './a';\nfunction use() { ns.f(); }\n",
            ),
        ];
        let blank = calls_from_edited(&files, "use", |local| {
            if let Some(aliases) = local.export_aliases.take() {
                let mut aliases = aliases.into_vec();
                for (exported, local_name) in [(" ", "g"), ("f", "  "), ("", "")] {
                    aliases.push(RawExportAlias {
                        exported: exported.into(),
                        local: local_name.into(),
                    });
                }
                local.export_aliases = Some(aliases.into_boxed_slice());
            }
        });
        assert_eq!(blank, vec![call("g", &module, 2, 95)], "{ext}: blank");
        let empty = calls_from_edited(&files, "use", |local| {
            if local.export_aliases.is_some() {
                local.export_aliases = Some(Vec::new().into_boxed_slice());
            }
        });
        assert_eq!(empty, Vec::<Call>::new(), "{ext}: empty");
    }
}

/// A barrel's `export *` reaches `a`'s export `f`, which is `g`. With a
/// private `f` in `a` the hop bound that `f` before the fix; without one,
/// no file declared `f`, so the hop never ran and no edge was bound.
#[test]
fn test_resolve_call_named_import_through_star_reexport_binds_alias_target() {
    for (module_src, line) in [
        ("function f() {}\nfunction g() {}\nexport { g as f };\n", 2),
        ("function g() {}\nexport { g as f };\n", 1),
    ] {
        for ext in TS_JS {
            let module = format!("a.{ext}");
            let barrel = format!("index.{ext}");
            let importer = format!("b.{ext}");
            assert_eq!(
                calls_from(
                    &[
                        (&module, module_src),
                        (&barrel, "export * from './a';\n"),
                        (
                            &importer,
                            "import { f } from './index';\nfunction use() { f(); }\n"
                        ),
                    ],
                    "use",
                ),
                vec![call("g", &module, line, 95)],
                "{ext}: {module_src}"
            );
        }
    }
}

/// `export *` never re-exports a default export: `ns.default` through a
/// barrel binds nothing, though `a` has one.
#[test]
fn test_resolve_call_namespace_default_through_star_reexport_binds_no_edge() {
    for ext in TS_JS {
        let module = format!("a.{ext}");
        let barrel = format!("index.{ext}");
        let importer = format!("b.{ext}");
        assert_eq!(
            calls_from(
                &[
                    (&module, "export default function h() {}\n"),
                    (&barrel, "export * from './a';\n"),
                    (
                        &importer,
                        "import * as ns from './index';\nfunction use() { ns.default(); }\n"
                    ),
                ],
                "use",
            ),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// FU-2026-10-15-a79b39506bec. `f` is private to the ES module `a` and the
/// only `f` in the repo: the import tier finds no export `f`, and the global
/// tier must not bind the private one. Before the fix it did, at 0.7.
#[test]
fn test_resolve_call_named_import_of_unique_private_declaration_binds_no_edge() {
    for ext in TS_JS {
        assert_eq!(
            named_import_calls(ext, "function f() {}\nexport function other() {}\n", "f"),
            Vec::<Call>::new(),
            "{ext}"
        );
    }
}

/// With no import naming `a`, the global tier keeps binding the unique `f`.
#[test]
fn test_resolve_call_unique_private_declaration_without_import_keeps_global_tier() {
    for ext in TS_JS {
        assert_eq!(
            importer_calls(
                ext,
                "function f() {}\nexport function other() {}\n",
                "function use() { f(); }\n",
            ),
            vec![call("f", &format!("a.{ext}"), 1, 70)],
            "{ext}"
        );
    }
}

/// An npm import names no repo file, so the global tier keeps binding the
/// repo's unique `f`, as before the fix.
#[test]
fn test_resolve_call_npm_import_of_privately_declared_name_keeps_global_tier() {
    for ext in TS_JS {
        assert_eq!(
            importer_calls(
                ext,
                "function f() {}\nexport function other() {}\n",
                "import { f } from 'lodash';\nfunction use() { f(); }\n",
            ),
            vec![call("f", &format!("a.{ext}"), 1, 70)],
            "{ext}"
        );
    }
}

/// A `.d.ts` file proves no export status (TS may export its declarations
/// implicitly), so an import of it keeps the by-name lookup.
#[test]
fn test_resolve_call_named_import_of_declaration_file_keeps_by_name_binding() {
    assert_eq!(
        calls_from(
            &[
                ("a.d.ts", "function f() {}\nexport function other() {}\n"),
                (
                    "b.ts",
                    "import { f } from './a.d';\nfunction use() { f(); }\n"
                ),
            ],
            "use",
        ),
        vec![call("f", "a.d.ts", 1, 95)]
    );
}

/// Python has no export statement: `from helpers import _helper` binds the
/// underscore-private function at the import tier, as before the fix.
#[test]
fn test_resolve_call_python_from_import_of_underscore_function_keeps_binding() {
    assert_eq!(
        calls_from(
            &[
                ("helpers.py", "def _helper():\n    pass\n"),
                ("other.py", "def _helper():\n    pass\n"),
                (
                    "app.py",
                    "from helpers import _helper\n\n\ndef go():\n    _helper()\n"
                ),
            ],
            "go",
        ),
        vec![call("_helper", "helpers.py", 1, 95)]
    );
}

/// The parsers record export names that differ from their declaration, with
/// every other export of the file, and nothing for a file without one. An
/// expression default records `default` with an empty local.
#[test]
fn test_parse_file_export_alias_records_pairs() {
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let js = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let alias = |exported: &str, local: &str| RawExportAlias {
        exported: exported.into(),
        local: local.into(),
    };
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        let parse = |src: &str| {
            provider
                .parse_file(Path::new(&format!("a.{ext}")), src.as_bytes())
                .expect("parse_file")
                .export_aliases
                .map(Vec::from)
        };
        assert_eq!(
            parse(
                "function g() {}\nexport function k() {}\nexport { g as f };\nexport default g;\n"
            ),
            Some(vec![
                alias("default", "g"),
                alias("f", "g"),
                alias("k", "k")
            ]),
            "{ext}"
        );
        assert_eq!(
            parse("export function k() {}\nfunction f() {}\nexport { f };\n"),
            None,
            "{ext}"
        );
        assert_eq!(
            parse("export const config = 1;\nfunction h() {}\nexport default wrap(h);\n"),
            Some(vec![alias("config", "config"), alias("default", "")]),
            "{ext}: an expression default binds no declaration"
        );
        assert_eq!(parse(""), None, "{ext}: empty file");
    }
}

/// A default import is marked [`BindingKind::EsDefault`]; a named import of
/// the same module is not.
#[test]
fn test_parse_file_default_import_marks_es_default() {
    let ts = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let js = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    for (provider, ext) in [
        (&ts as &dyn LanguageProvider, "ts"),
        (&js as &dyn LanguageProvider, "js"),
    ] {
        let imports = provider
            .parse_file(
                Path::new(&format!("b.{ext}")),
                b"import d, { e } from './a';\nimport * as ns from './a';\n",
            )
            .expect("parse_file")
            .imports;
        let kind_of = |name: &str| {
            imports
                .iter()
                .find(|import| import.imported_name == name && import.alias.is_none())
                .map(|import| import.binding_kind)
        };
        assert_eq!(kind_of("d"), Some(Some(BindingKind::EsDefault)), "{ext}: d");
        assert_eq!(kind_of("e"), Some(None), "{ext}: e");
    }
}
