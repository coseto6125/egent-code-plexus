//! Duplicated source trees: the same files under `core/` and under
//! `android/core/` (guava's layout; also vendored copies and monorepo
//! packages publishing one module). Every call to a duplicated declaration
//! used to be ambiguous and lost its edge. A call whose candidates are all
//! copies of one declaration now binds the copy in the caller's own tree,
//! as a tagged heuristic edge: `call:nearest-copy` at confidence 0.6.
//!
//! Each fixture writes its copy files and its caller files into both trees,
//! so the caller in each tree must bind the copy in that same tree.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::{NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const TREES: [&str; 2] = ["core", "android/core"];

fn provider(path: &str) -> Option<Box<dyn LanguageProvider>> {
    let ext = Path::new(path).extension()?.to_str()?;
    Some(match ext {
        "java" => Box::new(ecp_analyzer::java::parser::JavaProvider::new().unwrap()),
        "kt" => Box::new(ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap()),
        "cs" => Box::new(ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap()),
        "php" => Box::new(ecp_analyzer::php::parser::PhpProvider::new().unwrap()),
        "py" => Box::new(ecp_analyzer::python::PythonProvider::new().unwrap()),
        "rb" => Box::new(ecp_analyzer::ruby::parser::RubyProvider::new().unwrap()),
        "swift" => Box::new(ecp_analyzer::swift::parser::SwiftProvider::new().unwrap()),
        "dart" => Box::new(ecp_analyzer::dart::parser::DartProvider::new().unwrap()),
        "go" => Box::new(ecp_analyzer::go::parser::GoProvider::new().unwrap()),
        "rs" => Box::new(ecp_analyzer::rust::parser::RustProvider::new().unwrap()),
        "ts" => Box::new(ecp_analyzer::typescript::TypeScriptProvider::new().unwrap()),
        "js" => Box::new(ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap()),
        "c" => Box::new(ecp_analyzer::c::parser::CProvider::new().unwrap()),
        "cpp" => Box::new(ecp_analyzer::cpp::parser::CppProvider::new().unwrap()),
        _ => return None,
    })
}

/// Index `files` (repo-relative) from a temporary repo root, so manifests
/// such as `Cargo.toml` are on disk as the indexer sees them.
fn build(files: &[(String, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (rel, src) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
        if let Some(provider) = provider(rel) {
            builder.add_graph(
                provider
                    .parse_file(Path::new(rel), src.as_bytes())
                    .expect("parse_file"),
            );
        }
    }
    builder.build()
}

/// `(caller file, callee file, confidence, reason)` of every Calls edge
/// from a node named `caller` to a node named `callee`, sorted.
fn calls(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<(String, String, f32, String)> {
    let pool = &graph.string_pool;
    let file = |id: u32| {
        let idx = graph.nodes[id as usize].file_idx as usize;
        graph.files[idx].path.resolve(pool).to_string()
    };
    let mut out: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| graph.nodes[e.source as usize].name.resolve(pool) == caller)
        .filter(|e| graph.nodes[e.target as usize].name.resolve(pool) == callee)
        .map(|e| {
            (
                file(e.source),
                file(e.target),
                e.confidence,
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out
}

/// Both trees hold `copies` and `callers` (paths relative to a tree);
/// `shared` files sit once at the repo root. `t` in each tree's caller
/// must call `callee` in that tree's copy, as a nearest-copy edge.
fn assert_each_tree_binds_its_copy(
    copies: &[(&str, &str)],
    callers: &[(&str, &str)],
    shared: &[(&str, &str)],
    callee: &str,
) {
    let mut files: Vec<(String, &str)> = shared.iter().map(|&(p, s)| (p.to_string(), s)).collect();
    for tree in TREES {
        for &(rel, src) in copies.iter().chain(callers) {
            files.push((format!("{tree}/{rel}"), src));
        }
    }
    let graph = build(&files);
    let found = calls(&graph, "t", callee);
    assert_eq!(
        found.len(),
        TREES.len() * callers.len(),
        "one edge per caller: {found:?}"
    );
    for (caller_file, callee_file, confidence, reason) in &found {
        let tree = if caller_file.starts_with("android/") {
            "android/core/"
        } else {
            "core/"
        };
        assert!(
            callee_file.starts_with(tree),
            "{caller_file} binds its own tree's copy, got {callee_file}"
        );
        assert_eq!(*confidence, NEAREST_COPY_CONFIDENCE, "{caller_file}");
        assert_eq!(reason, NEAREST_COPY_REASON, "{caller_file}");
    }
}

/// guava: a test in the same package as `Maps`, no import, `Maps.make()`.
/// The receiver-typing ladder finds two `Maps` that both own `make`.
#[test]
fn test_resolve_call_java_duplicate_tree_same_package_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "src/p/Maps.java",
            "package p;\npublic class Maps { public static void make() {} }\n",
        )],
        &[(
            "test/p/MapsTest.java",
            "package p;\nclass MapsTest { void t() { Maps.make(); } }\n",
        )],
        &[],
        "make",
    );
}

#[test]
fn test_resolve_call_kotlin_duplicate_tree_object_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "src/p/KMaps.kt",
            "package p\nobject KMaps { fun make() {} }\n",
        )],
        &[(
            "test/p/KMapsTest.kt",
            "package p\nclass KMapsTest { fun t() { KMaps.make() } }\n",
        )],
        &[],
        "make",
    );
}

#[test]
fn test_resolve_call_csharp_duplicate_tree_static_class_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "src/p/CMaps.cs",
            "namespace P { public static class CMaps { public static void Make() {} } }\n",
        )],
        &[(
            "test/p/CMapsTest.cs",
            "namespace P { class CMapsTest { void t() { CMaps.Make(); } } }\n",
        )],
        &[],
        "Make",
    );
}

#[test]
fn test_resolve_call_php_duplicate_tree_static_call_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "src/p/PMaps.php",
            "<?php\nnamespace P;\nclass PMaps { public static function make() {} }\n",
        )],
        &[(
            "test/p/PMapsTest.php",
            "<?php\nnamespace P;\nclass PMapsTest { function t() { PMaps::make(); } }\n",
        )],
        &[],
        "make",
    );
}

/// The absolute import names a package both trees hold.
#[test]
fn test_resolve_call_python_duplicate_tree_absolute_import_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[
            ("py/pkg/__init__.py", ""),
            (
                "py/pkg/maps.py",
                "class PyMaps:\n    @staticmethod\n    def make():\n        pass\n",
            ),
        ],
        &[(
            "py/tests/test_maps.py",
            "from pkg.maps import PyMaps\ndef t():\n    PyMaps.make()\n",
        )],
        &[],
        "make",
    );
}

#[test]
fn test_resolve_call_ruby_duplicate_tree_constant_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "rb/lib/rmaps.rb",
            "class RMaps\n  def self.make\n  end\nend\n",
        )],
        &[(
            "rb/test/rmaps_test.rb",
            "class RMapsTest\n  def t\n    RMaps.make\n  end\nend\n",
        )],
        &[],
        "make",
    );
}

#[test]
fn test_resolve_call_swift_duplicate_tree_module_import_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "sw/Sources/Dup/SMaps.swift",
            "public class SMaps { public static func make() {} }\n",
        )],
        &[(
            "sw/Tests/SMapsTests.swift",
            "import Dup\nclass SMapsTests { func t() { SMaps.make() } }\n",
        )],
        &[],
        "make",
    );
}

#[test]
fn test_resolve_call_dart_duplicate_tree_package_import_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "dart/lib/dmaps.dart",
            "class DMaps { static void make() {} }\n",
        )],
        &[(
            "dart/test/dmaps_test.dart",
            "import 'package:dup/dmaps.dart';\nvoid t() { DMaps.make(); }\n",
        )],
        &[],
        "make",
    );
}

/// Two copies of one Go module: the import path names both.
#[test]
fn test_resolve_call_go_duplicate_module_binds_nearest_copy() {
    for module in ["example.com/dup", "dup"] {
        let manifest = format!("module {module}\n\ngo 1.21\n");
        let caller =
            format!("package main\n\nimport \"{module}/pkg\"\n\nfunc t() {{ pkg.Make() }}\n");
        assert_each_tree_binds_its_copy(
            &[
                ("go/go.mod", &manifest),
                ("go/pkg/pkg.go", "package pkg\n\nfunc Make() {}\n"),
            ],
            &[("go/cmd/main.go", &caller)],
            &[],
            "Make",
        );
    }
}

/// The workspace holds `core/dup` and `core/app`; the `android/` copy of
/// the crate sits outside it, as Cargo allows one package per name.
#[test]
fn test_resolve_call_rust_duplicate_crate_import_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[
            ("dup/Cargo.toml", "[package]\nname = \"dup\"\nversion = \"0.1.0\"\n"),
            ("dup/src/lib.rs", "pub fn rs_make() {}\n"),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ndup = { path = \"../dup\" }\n",
            ),
        ],
        &[("app/src/lib.rs", "use dup::rs_make;\npub fn t() { rs_make(); }\n")],
        &[(
            "Cargo.toml",
            "[workspace]\nmembers = [\"core/app\", \"core/dup\"]\n",
        )],
        "rs_make",
    );
}

/// Two copies of one workspace package: the bare specifier names both.
#[test]
fn test_resolve_call_typescript_duplicate_package_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[
            (
                "tsp/pkgx/package.json",
                "{\"name\":\"pkgx\",\"main\":\"src/index.ts\"}\n",
            ),
            ("tsp/pkgx/src/index.ts", "export function tsMake() {}\n"),
        ],
        &[(
            "tsp/app/use.ts",
            "import { tsMake } from 'pkgx';\nexport function t() { tsMake(); }\n",
        )],
        &[],
        "tsMake",
    );
}

#[test]
fn test_resolve_call_javascript_duplicate_package_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[
            (
                "jsp/pkgx/package.json",
                "{\"name\":\"pkgx\",\"main\":\"src/index.js\"}\n",
            ),
            ("jsp/pkgx/src/index.js", "export function jsMake() {}\n"),
        ],
        &[(
            "jsp/app/use.js",
            "import { jsMake } from 'pkgx';\nexport function t() { jsMake(); }\n",
        )],
        &[],
        "jsMake",
    );
}

/// The same `.c` file in two trees.
#[test]
fn test_resolve_call_c_duplicate_tree_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[("c/src/cm.c", "void c_make(void) {}\n")],
        &[("c/test/t.c", "void t(void) { c_make(); }\n")],
        &[],
        "c_make",
    );
}

#[test]
fn test_resolve_call_cpp_duplicate_tree_static_member_binds_nearest_copy() {
    assert_each_tree_binds_its_copy(
        &[(
            "cpp/src/cmaps.cpp",
            "class CppMaps { public: static void make() {} };\n",
        )],
        &[("cpp/test/t.cpp", "void t() { CppMaps::make(); }\n")],
        &[],
        "make",
    );
}

/// Two copies at the same distance from the caller: no copy is nearer, so
/// the call stays unresolved.
#[test]
fn test_resolve_call_java_equidistant_copies_emit_no_edge() {
    let java = "package p;\npublic class Maps { public static void make() {} }\n";
    let graph = build(&[
        ("left/src/p/Maps.java".into(), java),
        ("right/src/p/Maps.java".into(), java),
        (
            "mid/test/p/MapsTest.java".into(),
            "package p;\nclass MapsTest { void t() { Maps.make(); } }\n",
        ),
    ]);
    assert!(calls(&graph, "t", "make").is_empty());
}

#[test]
fn test_resolve_call_go_equidistant_copies_emit_no_edge() {
    let go = "package pkg\n\nfunc Make() {}\n";
    let graph = build(&[
        ("left/pkg/pkg.go".into(), go),
        ("right/pkg/pkg.go".into(), go),
        (
            "mid/cmd/main.go".into(),
            "package main\n\nimport \"example.com/dup/pkg\"\n\nfunc t() { pkg.Make() }\n",
        ),
    ]);
    assert!(calls(&graph, "t", "Make").is_empty());
}

/// Declarations in files of different names are two declarations, not
/// copies: the call stays ambiguous whatever the paths.
#[test]
fn test_resolve_call_go_same_name_in_different_files_emits_no_edge() {
    let graph = build(&[
        ("core/pkg/a.go".into(), "package pkg\n\nfunc Make() {}\n"),
        (
            "android/core/pkg/b.go".into(),
            "package pkg\n\nfunc Make() {}\n",
        ),
        (
            "core/cmd/main.go".into(),
            "package main\n\nimport \"example.com/dup/pkg\"\n\nfunc t() { pkg.Make() }\n",
        ),
    ]);
    assert!(calls(&graph, "t", "Make").is_empty());
}

/// Java copies declared in different packages are different classes.
#[test]
fn test_resolve_call_java_same_class_name_in_other_package_emits_no_edge() {
    let graph = build(&[
        (
            "core/src/p/Maps.java".into(),
            "package p;\npublic class Maps { public static void make() {} }\n",
        ),
        (
            "android/core/src/q/Maps.java".into(),
            "package q;\npublic class Maps { public static void make() {} }\n",
        ),
        (
            "core/test/r/MapsTest.java".into(),
            "package r;\nclass MapsTest { void t() { Maps.make(); } }\n",
        ),
    ]);
    assert!(calls(&graph, "t", "make").is_empty());
}

/// Same-named files in directories of different names are two modules,
/// not copies: `a/util.py` and `b/util.py` both define `helper`.
#[test]
fn test_resolve_call_python_same_file_name_in_other_package_emits_no_edge() {
    let helper = "def helper():\n    pass\n";
    let graph = build(&[
        ("core/a/util.py".into(), helper),
        ("android/core/b/util.py".into(), helper),
        ("core/a/app.py".into(), "def t():\n    helper()\n"),
    ]);
    assert!(calls(&graph, "t", "helper").is_empty());
}
