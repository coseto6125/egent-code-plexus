//! `OverlayView` against a full reindex for three fidelity defects: the
//! builder-made nodes of a dirty file (FU-2026-10-08-264b182adee6), the
//! import tier over a dirty target file (FU-2026-10-08-7bd2cba6a72c), and
//! calls and constructions through an import alias.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlaySymbol, OverlayView};
use std::path::Path;

fn input(local: LocalGraph) -> OverlayFileInput {
    OverlayFileInput {
        rel_path: local.file_path.to_string_lossy().into_owned(),
        imports: local.imports,
        symbols: local
            .nodes
            .into_iter()
            .map(|n| OverlaySymbol {
                name: n.name,
                kind: n.kind,
                owner_class: n.owner_class,
                start_line: n.span.0 + 1,
                end_line: n.span.2 + 1,
                start_column: n.span.1,
                end_column: n.span.3,
                calls: n.calls,
            })
            .collect(),
    }
}

/// Index `files`, reindex them with `dirty` applied (the truth), and merge
/// the overlay of `dirty` over the first index; `check` gets both.
fn with_overlay(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    check: impl FnOnce(&ZeroCopyGraph, MergedGraph<'_>),
) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"overlay-fidelity\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let build = |edits: &[(&str, &str)]| {
        let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
        let mut sources = files.to_vec();
        sources.extend(
            edits
                .iter()
                .filter(|(p, _)| !files.iter().any(|(old, _)| old == p)),
        );
        for &(path, original) in &sources {
            let source = edits
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(original, |(_, s)| *s);
            let disk = tmp.path().join(path);
            std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
            std::fs::write(disk, source).unwrap();
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            );
        }
        builder.build()
    };
    let base = build(&[]);
    let full = build(dirty);
    let inputs: Vec<_> = dirty
        .iter()
        .map(|(path, source)| {
            input(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&base).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let view = OverlayView::build(archived, &inputs).unwrap();
    check(&full, MergedGraph::new(archived, Some(&view)));
}

type Target = (String, String, String, u32);

fn reindex_calls(graph: &ZeroCopyGraph, caller: &str) -> Vec<Target> {
    let pool = &graph.string_pool;
    let mut result: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::Calls
                && graph.nodes[e.source as usize].name.resolve(pool) == caller
        })
        .map(|e| {
            let n = &graph.nodes[e.target as usize];
            (
                graph.files[n.file_idx as usize].path.resolve(pool).into(),
                n.name.resolve(pool).into(),
                n.owner_class.resolve(pool).into(),
                (e.confidence * 100.0).round() as u32,
            )
        })
        .collect();
    result.sort();
    result.dedup();
    result
}

fn merged_calls(merged: MergedGraph<'_>, caller: &str) -> Vec<Target> {
    let mut result: Vec<Target> = merged
        .all_edges()
        .filter(|e| {
            e.rel_type() == RelType::Calls && merged.node(e.source).unwrap().name(&merged) == caller
        })
        .map(|e| {
            let n = merged.node(e.target).unwrap();
            (
                n.file_path(&merged).unwrap().into(),
                n.name(&merged).into(),
                n.owner_class(&merged).unwrap_or_default().into(),
                (e.confidence() * 100.0).round() as u32,
            )
        })
        .collect();
    result.sort();
    result.dedup();
    result
}

/// The reindex resolves `caller` to `expected`, and the merged overlay
/// resolves it the same way.
fn assert_calls(
    provider: &dyn LanguageProvider,
    files: &[(&str, &str)],
    dirty: &[(&str, &str)],
    caller: &str,
    expected: &[(&str, &str, &str, u32)],
) {
    let expected: Vec<Target> = expected
        .iter()
        .map(|&(file, name, owner, confidence)| {
            (file.into(), name.into(), owner.into(), confidence)
        })
        .collect();
    with_overlay(provider, files, dirty, |full, merged| {
        assert_eq!(
            reindex_calls(full, caller),
            expected,
            "full reindex fixture must exercise the intended target: {dirty:?}"
        );
        assert_eq!(
            merged_calls(merged, caller),
            expected,
            "merged overlay Calls must equal the reindex: {dirty:?}"
        );
    });
}

/// After `path` is edited from `before` to `after`, its one builder-made
/// `kind` node stays visible in the merged view, its `rel` edge still
/// reaches the surviving `keep` (redirected to the fresh parse), and the
/// deleted `gone` is reported nowhere.
fn assert_dirty_file_keeps(
    provider: &dyn LanguageProvider,
    (path, before, after): (&str, &str, &str),
    (kind, rel): (NodeKind, RelType),
    keep: Option<&str>,
    gone: Option<&str>,
) {
    with_overlay(
        provider,
        &[(path, before)],
        &[(path, after)],
        |full, merged| {
            let pool = &full.string_pool;
            let in_path = |idx: u32| {
                let n = &full.nodes[idx as usize];
                n.kind == kind && full.files[n.file_idx as usize].path.resolve(pool) == path
            };
            assert_eq!(
                (0..full.nodes.len() as u32).filter(|&i| in_path(i)).count(),
                1,
                "full reindex must hold one {kind:?} node for {path}"
            );
            let reindexed: Vec<&str> = full
                .edges
                .iter()
                .filter(|e| e.rel_type == rel && in_path(e.source))
                .map(|e| full.nodes[e.target as usize].name.resolve(pool))
                .collect();
            if let Some(keep) = keep {
                assert!(
                    reindexed.contains(&keep),
                    "full reindex fixture must link {kind:?} -{rel:?}-> {keep}: {reindexed:?}"
                );
            }

            let visible = |idx: &u32| *idx >= merged.base_len() || merged.base_visible(*idx);
            let markers: Vec<u32> = (0..merged.node_count())
                .filter(visible)
                .filter(|&idx| {
                    let n = merged.node(idx).unwrap();
                    n.kind() == kind && n.file_path(&merged) == Some(path)
                })
                .collect();
            assert_eq!(
                markers.len(),
                1,
                "the dirty file's {kind:?} node must stay visible: {path}"
            );
            let linked: Vec<(u32, &str)> = merged
                .out_edges(markers[0])
                .filter(|e| e.rel_type() == rel)
                .map(|e| (e.target, merged.node(e.target).unwrap().name(&merged)))
                .collect();
            if let Some(keep) = keep {
                assert!(
                    linked
                        .iter()
                        .any(|&(target, name)| name == keep && target >= merged.base_len()),
                    "{kind:?} -{rel:?}-> {keep} must reach the fresh parse: {linked:?}"
                );
            }
            if let Some(gone) = gone {
                assert!(
                    linked.iter().all(|&(_, name)| name != gone),
                    "{kind:?} -{rel:?}-> {gone} must drop with the deleted symbol: {linked:?}"
                );
                assert!(
                    !(0..merged.node_count()).filter(visible).any(|idx| {
                        let n = merged.node(idx).unwrap();
                        n.name(&merged) == gone && n.file_path(&merged) == Some(path)
                    }),
                    "the deleted {gone} must not be reported: {path}"
                );
            }
        },
    );
}

// ── U1: builder-made nodes of a dirty file ─────────────────────────────────
// Before the fix every base node of a dirty file that the fragment did not
// re-emit was suppressed, the File node included.

const FILE_DEFINES: (NodeKind, RelType) = (NodeKind::File, RelType::Defines);

macro_rules! file_node_kept {
    ($test:ident, $provider:expr, $path:literal, $before:literal, $after:literal, $keep:literal, $gone:literal) => {
        #[test]
        fn $test() {
            assert_dirty_file_keeps(
                &$provider,
                ($path, $before, $after),
                FILE_DEFINES,
                Some($keep),
                Some($gone),
            );
        }
    };
}

file_node_kept!(
    test_build_typescript_edited_file_keeps_file_node_defines,
    ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
    "src/app.ts",
    "export function keep() {}\nexport function gone() {}\n",
    "export function keep() { return 1; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_javascript_edited_file_keeps_file_node_defines,
    ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
    "src/app.js",
    "function keep() {}\nfunction gone() {}\n",
    "function keep() { return 1; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_python_edited_file_keeps_file_node_defines,
    ecp_analyzer::python::PythonProvider::new().unwrap(),
    "app.py",
    "def keep():\n    pass\n\n\ndef gone():\n    pass\n",
    "def keep():\n    return 1\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_java_edited_file_keeps_file_node_defines,
    ecp_analyzer::java::JavaProvider::new().unwrap(),
    "App.java",
    "class Keep {}\nclass Gone {}\n",
    "class Keep { void run() {} }\n",
    "Keep",
    "Gone"
);
file_node_kept!(
    test_build_kotlin_edited_file_keeps_file_node_defines,
    ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
    "App.kt",
    "fun keep() {}\nfun gone() {}\n",
    "fun keep() { println(1) }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_csharp_edited_file_keeps_file_node_defines,
    ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
    "App.cs",
    "class Keep {}\nclass Gone {}\n",
    "class Keep { void Run() {} }\n",
    "Keep",
    "Gone"
);
file_node_kept!(
    test_build_go_edited_file_keeps_file_node_defines,
    ecp_analyzer::go::parser::GoProvider::new().unwrap(),
    "app.go",
    "package demo\n\nfunc Keep() {}\n\nfunc Gone() {}\n",
    "package demo\n\nfunc Keep() { println(1) }\n",
    "Keep",
    "Gone"
);
file_node_kept!(
    test_build_rust_edited_file_keeps_file_node_defines,
    ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
    "src/lib.rs",
    "pub fn keep() {}\npub fn gone() {}\n",
    "pub fn keep() { let _ = 1; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_php_edited_file_keeps_file_node_defines,
    ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
    "app.php",
    "<?php\nfunction keep() {}\nfunction gone() {}\n",
    "<?php\nfunction keep() { return 1; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_ruby_edited_file_keeps_file_node_defines,
    ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
    "app.rb",
    "def keep\nend\n\ndef gone\nend\n",
    "def keep\n  1\nend\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_swift_edited_file_keeps_file_node_defines,
    ecp_analyzer::swift::SwiftProvider::new().unwrap(),
    "App.swift",
    "func keep() {}\nfunc gone() {}\n",
    "func keep() { print(1) }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_c_edited_file_keeps_file_node_defines,
    ecp_analyzer::c::CProvider::new().unwrap(),
    "app.c",
    "void keep(void) {}\nvoid gone(void) {}\n",
    "void keep(void) { return; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_cpp_edited_file_keeps_file_node_defines,
    ecp_analyzer::cpp::CppProvider::new().unwrap(),
    "app.cpp",
    "void keep() {}\nvoid gone() {}\n",
    "void keep() { return; }\n",
    "keep",
    "gone"
);
file_node_kept!(
    test_build_dart_edited_file_keeps_file_node_defines,
    ecp_analyzer::dart::DartProvider::new().unwrap(),
    "lib/app.dart",
    "void keep() {}\nvoid gone() {}\n",
    "void keep() { print(1); }\n",
    "keep",
    "gone"
);

#[test]
fn test_build_emptied_file_keeps_file_node_without_defines() {
    assert_dirty_file_keeps(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        ("src/app.ts", "export function gone() {}\n", ""),
        FILE_DEFINES,
        None,
        Some("gone"),
    );
}

#[test]
fn test_build_edited_main_keeps_entry_point_reference() {
    assert_dirty_file_keeps(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        (
            "src/main.rs",
            "fn main() {}\n",
            "fn main() { let _ = 1; }\n",
        ),
        (NodeKind::EntryPoint, RelType::References),
        Some("main"),
        None,
    );
}

// ── U2: the import tier over a dirty target file ───────────────────────────
// Python, Java, Kotlin, PHP: n/a — their import tier (`bind_import`) already
// reads a dirty module from its fresh parse.
// Go, Swift, Dart, C++: n/a — an import binds a package, module or library
// (and C++ `#include` / namespace alias a file), never a symbol by name.
// C: n/a — `#include` binds no name; typedef and macro bindings alias only
// themselves. Ruby: n/a — `require` binds a file, `alias` only itself.
// C#: n/a — a `using` names a namespace or a qualified type path, which the
// named-import tier looks up as written and never finds in either build.

const TS_HELPER: (&str, &str) = ("src/a.ts", "export function helper() {}\n");
const TS_NAMESAKE: (&str, &str) = ("src/c.ts", "export function helper() {}\n");
const TS_IMPORTER: &str = "import { helper } from './a';\nexport function useIt() { return 1; }\n";
const TS_IMPORTER_CALLS: &str =
    "import { helper } from './a';\nexport function useIt() { return helper(); }\n";

#[test]
fn test_build_typescript_dirty_importer_of_dirty_module_binds_import_scoped() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[TS_HELPER, TS_NAMESAKE, ("src/b.ts", TS_IMPORTER)],
        &[
            ("src/a.ts", "export function helper() { return 1; }\n"),
            ("src/b.ts", TS_IMPORTER_CALLS),
        ],
        "useIt",
        &[("src/a.ts", "helper", "", 95)],
    );
}

#[test]
fn test_build_javascript_dirty_importer_of_dirty_module_binds_import_scoped() {
    assert_calls(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        &[
            ("src/a.js", "export function helper() {}\n"),
            ("src/c.js", "export function helper() {}\n"),
            (
                "src/b.js",
                "import { helper } from './a';\nexport function useIt() { return 1; }\n",
            ),
        ],
        &[
            ("src/a.js", "export function helper() { return 1; }\n"),
            (
                "src/b.js",
                "import { helper } from './a';\nexport function useIt() { return helper(); }\n",
            ),
        ],
        "useIt",
        &[("src/a.js", "helper", "", 95)],
    );
}

#[test]
fn test_build_rust_dirty_importer_of_dirty_module_binds_import_scoped() {
    assert_calls(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
            ("src/a.rs", "pub fn helper() {}\n"),
            ("src/c.rs", "pub fn helper() {}\n"),
            ("src/b.rs", "use crate::a::helper;\npub fn use_it() {}\n"),
        ],
        &[
            ("src/a.rs", "pub fn helper() { let _ = 1; }\n"),
            (
                "src/b.rs",
                "use crate::a::helper;\npub fn use_it() { helper(); }\n",
            ),
        ],
        "use_it",
        &[("src/a.rs", "helper", "", 95)],
    );
}

// Guard, not a defect repro: the deleted `helper` of the dirty module is a
// suppressed base node, which the import tier must never bind. The reindex
// falls through to the one remaining `helper`.
#[test]
fn test_build_typescript_import_of_deleted_symbol_skips_suppressed_node() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[TS_HELPER, TS_NAMESAKE, ("src/b.ts", TS_IMPORTER)],
        &[
            ("src/a.ts", "export function other() {}\n"),
            ("src/b.ts", TS_IMPORTER_CALLS),
        ],
        "useIt",
        &[("src/c.ts", "helper", "", 70)],
    );
}

// Guard, not a defect repro: a clean importer keeps its index-time edge,
// redirected into the dirty module's fresh parse.
#[test]
fn test_build_typescript_clean_importer_of_dirty_module_redirects_base_edge() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[TS_HELPER, TS_NAMESAKE, ("src/b.ts", TS_IMPORTER_CALLS)],
        &[("src/a.ts", "export function helper() { return 1; }\n")],
        "useIt",
        &[("src/a.ts", "helper", "", 95)],
    );
}

// Guard, not a defect repro: a dirty importer of a clean module.
#[test]
fn test_build_typescript_dirty_importer_of_clean_module_binds_import_scoped() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[TS_HELPER, TS_NAMESAKE, ("src/b.ts", TS_IMPORTER)],
        &[("src/b.ts", TS_IMPORTER_CALLS)],
        "useIt",
        &[("src/a.ts", "helper", "", 95)],
    );
}

// ── G4: calls and constructions through an import alias ────────────────────
// Calls through an alias already resolve in Python, Kotlin and PHP
// (`import_binding` maps the alias); only their constructions are tested.
// Java: n/a — no import alias syntax.
// Go, Dart: n/a — an alias names a package or library prefix, not a symbol.
// C#: n/a — `using G = Ns.Gadget;` records the qualified path as the
// imported name, which neither build resolves.
// Swift, Ruby, C: n/a — `typealias`, `alias` and typedef bindings alias only
// themselves, so the declared name is the alias.
// C++: n/a — a namespace alias names a namespace, not a symbol.
// Rust: no constructors; only the call is tested.

#[test]
fn test_build_typescript_call_through_import_alias_binds_declared_function() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[
            TS_HELPER,
            (
                "src/app.ts",
                "import { helper as h } from './a';\nexport function run() { return 1; }\n",
            ),
        ],
        &[(
            "src/app.ts",
            "import { helper as h } from './a';\nexport function run() { return h(); }\n",
        )],
        "run",
        &[("src/a.ts", "helper", "", 95)],
    );
}

#[test]
fn test_build_typescript_construction_through_import_alias_calls_declared_class() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[
            ("src/gadget.ts", "export class Gadget {}\n"),
            (
                "src/app.ts",
                "import { Gadget as G } from './gadget';\nexport function make() { return 1; }\n",
            ),
        ],
        &[(
            "src/app.ts",
            "import { Gadget as G } from './gadget';\nexport function make() { return new G(); }\n",
        )],
        "make",
        &[("src/gadget.ts", "Gadget", "", 95)],
    );
}

// The alias shares its name with another file's class: the import still
// binds the declared `Gadget`, never the namesake `Widget`.
#[test]
fn test_build_typescript_alias_named_like_other_class_binds_imported_class() {
    assert_calls(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[
            ("src/gadget.ts", "export class Gadget {}\n"),
            ("src/widget.ts", "export class Widget {}\n"),
            (
                "src/app.ts",
                "import { Gadget as Widget } from './gadget';\nexport function make() { return 1; }\n",
            ),
        ],
        &[(
            "src/app.ts",
            "import { Gadget as Widget } from './gadget';\nexport function make() { return new Widget(); }\n",
        )],
        "make",
        &[("src/gadget.ts", "Gadget", "", 95)],
    );
}

#[test]
fn test_build_javascript_call_through_import_alias_binds_declared_function() {
    assert_calls(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        &[
            ("src/a.js", "export function helper() {}\n"),
            (
                "src/app.js",
                "import { helper as h } from './a';\nexport function run() { return 1; }\n",
            ),
        ],
        &[(
            "src/app.js",
            "import { helper as h } from './a';\nexport function run() { return h(); }\n",
        )],
        "run",
        &[("src/a.js", "helper", "", 95)],
    );
}

#[test]
fn test_build_javascript_construction_through_import_alias_calls_declared_class() {
    assert_calls(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        &[
            ("src/gadget.js", "export class Gadget {}\n"),
            (
                "src/app.js",
                "import { Gadget as G } from './gadget';\nexport function make() { return 1; }\n",
            ),
        ],
        &[(
            "src/app.js",
            "import { Gadget as G } from './gadget';\nexport function make() { return new G(); }\n",
        )],
        "make",
        &[("src/gadget.js", "Gadget", "", 95)],
    );
}

#[test]
fn test_build_rust_call_through_use_alias_binds_declared_function() {
    assert_calls(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[
            ("src/lib.rs", "mod a;\nmod b;\n"),
            ("src/a.rs", "pub fn helper() {}\n"),
            (
                "src/b.rs",
                "use crate::a::{helper as h};\npub fn run() {}\n",
            ),
        ],
        &[(
            "src/b.rs",
            "use crate::a::{helper as h};\npub fn run() { h(); }\n",
        )],
        "run",
        &[("src/a.rs", "helper", "", 95)],
    );
}

#[test]
fn test_build_python_construction_through_import_alias_calls_constructor() {
    assert_calls(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        &[
            (
                "pkg/widget.py",
                "class Widget:\n    def __init__(self, x):\n        self.x = x\n",
            ),
            (
                "pkg/app.py",
                "from pkg.widget import Widget as W\n\n\ndef make_aliased():\n    return 1\n",
            ),
        ],
        &[(
            "pkg/app.py",
            "from pkg.widget import Widget as W\n\n\ndef make_aliased():\n    return W(1)\n",
        )],
        "make_aliased",
        &[("pkg/widget.py", "__init__", "Widget", 95)],
    );
}

#[test]
fn test_build_kotlin_construction_through_import_alias_calls_declared_class() {
    assert_calls(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        &[
            (
                "src/main/kotlin/pkg/Gadget.kt",
                "package pkg\nclass Gadget {\n}\n",
            ),
            ("App.kt", "import pkg.Gadget as G\nfun run() {}\n"),
        ],
        &[("App.kt", "import pkg.Gadget as G\nfun run() { G() }\n")],
        "run",
        &[("src/main/kotlin/pkg/Gadget.kt", "Gadget", "", 95)],
    );
}

#[test]
fn test_build_php_construction_through_use_alias_calls_declared_class() {
    assert_calls(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        &[
            ("src/pkg/Helper.php", "<?php namespace pkg; class Helper {}"),
            (
                "src/other/Helper.php",
                "<?php namespace other; class Helper {}",
            ),
            ("app.php", "<?php use pkg\\Helper as H; function run() {}"),
        ],
        &[(
            "app.php",
            "<?php use pkg\\Helper as H; function run() { new H(); }",
        )],
        "run",
        &[("src/pkg/Helper.php", "Helper", "", 95)],
    );
}
