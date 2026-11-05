//! A function passed as a value through a module, namespace, package or
//! class qualifier (`map(mod.f, xs)`, `xs.map(ns.f)`, `register(pkg.F)`) gets
//! a `References` edge with reason `function_reference`, like the bare
//! `map(f, xs)` form. The qualifier picks the declaration: with `a.run` and
//! `b.run` in the tree, `sink(a.run)` binds only `a`.
//!
//! The grammar cannot tell a module from an instance (`obj.f`), so the parser
//! records every `qualifier.member` value and the resolver binds it only
//! when the qualifier resolves to a module, namespace, package or class. A
//! callee (`mod.f()`) stays a `Calls` edge only, and a variable qualifier
//! keeps its own value read.
//!
//! Go and Python bind a call through call-only package and submodule
//! lookups. A value reference takes the shared qualifier tiers instead, so
//! the Go cases name the shape the tiers reach today.
use crate::name_ref_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::ZeroCopyGraph;
use name_ref_support::{assert_one_cross_file, function_refs, graph, value_reads};
use std::path::Path;

fn none() -> Vec<name_ref_support::Ref> {
    Vec::new()
}

fn py(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph(&PythonProvider::new().unwrap(), files)
}

fn ts(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph(&TypeScriptProvider::new().unwrap(), files)
}

fn js(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph(&JavaScriptProvider::new().unwrap(), files)
}

/// A Go module fixture with the alias the CLI derives from its `go.mod`.
fn go_module(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().expect("tempdir");
    let provider = GoProvider::new().expect("GoProvider init");
    let mut aliases = PathAliases::new();
    aliases.add("example.com/app/*", vec!["./*".to_string()]);
    let mut builder = GraphBuilder::new()
        .with_repo_root(tmp.path().to_path_buf())
        .with_path_aliases(aliases);
    for &(path, source) in files {
        let disk = tmp.path().join(path);
        std::fs::create_dir_all(disk.parent().expect("parent")).expect("create parent");
        std::fs::write(&disk, source).expect("write fixture");
        if path.ends_with(".go") {
            builder.add_graph(
                provider
                    .parse_file(Path::new(path), source.as_bytes())
                    .expect("parse Go fixture"),
            );
        }
    }
    builder.build()
}

const GO_MOD: (&str, &str) = ("go.mod", "module example.com/app\n\ngo 1.21\n");

// ---- Python: `mod.f` after `import mod`, `from pkg import mod`, `Util.f` ----

#[test]
fn test_qualified_function_reference_python_module_argument_binds_member() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "import mod\n\ndef run(items):\n    return map(mod.f, items)\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "run"), "mod.py", "f");
}

#[test]
fn test_qualified_function_reference_python_from_package_import_module_binds_member() {
    let g = py(&[
        ("pkg/__init__.py", ""),
        ("pkg/mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "from pkg import mod\n\ndef run(items):\n    return map(mod.f, items)\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "run"), "pkg/mod.py", "f");
}

#[test]
fn test_qualified_function_reference_python_class_static_argument_binds_method() {
    let g = py(&[(
        "app.py",
        "class Util:\n    @staticmethod\n    def f(x):\n        return x\n\ndef run(items):\n    return map(Util.f, items)\n",
    )]);
    let edges = function_refs(&g, "run");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(
        (edges[0].0.as_str(), edges[0].1.as_str()),
        ("app.py", "f"),
        "{edges:?}"
    );
}

#[test]
fn test_qualified_function_reference_python_callee_returns_no_function_reference() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        ("app.py", "import mod\n\ndef run():\n    return mod.f(1)\n"),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_python_parameter_qualifier_returns_no_edge() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "import mod\n\ndef run(obj, items):\n    return map(obj.f, items)\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_python_variable_qualifier_keeps_value_read() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "import mod\n\nLIMITS = object()\n\ndef run(items):\n    return map(LIMITS.f, items)\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
    assert!(
        value_reads(&g, "run")
            .iter()
            .any(|(_, name, ..)| name == "LIMITS"),
        "{:?}",
        value_reads(&g, "run")
    );
}

#[test]
fn test_qualified_function_reference_python_same_member_in_two_modules_binds_named_one() {
    let g = py(&[
        ("a.py", "def run(x):\n    return x\n"),
        ("b.py", "def run(x):\n    return x\n"),
        (
            "app.py",
            "import a\nimport b\n\ndef go(items):\n    return map(a.run, items)\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "a.py", "run");
}

#[test]
fn test_qualified_function_reference_python_unresolvable_qualifier_returns_no_edge() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "def run(items):\n    return map(missing.f, items)\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_python_self_qualifier_returns_no_edge() {
    let g = py(&[
        ("mod.py", "def f(x):\n    return x\n"),
        (
            "app.py",
            "import mod\n\nclass C:\n    def f(self, x):\n        return x\n\n    def run(self, items):\n        return map(self.f, items)\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- TypeScript: `ns.f` after `import * as ns`, `Util.f` ----

const TS_LIB: (&str, &str) = (
    "lib.ts",
    "export function f(x: number): number {\n  return x;\n}\n",
);

#[test]
fn test_qualified_function_reference_typescript_namespace_argument_binds_member() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items.map(ns.f);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "run"), "lib.ts", "f");
}

#[test]
fn test_qualified_function_reference_typescript_class_static_argument_binds_method() {
    let g = ts(&[(
        "app.ts",
        "class Util {\n  static f(x: number): number {\n    return x;\n  }\n}\nexport function run(items: number[]): number[] {\n  return items.map(Util.f);\n}\n",
    )]);
    let edges = function_refs(&g, "run");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(
        (edges[0].0.as_str(), edges[0].1.as_str()),
        ("app.ts", "f"),
        "{edges:?}"
    );
}

#[test]
fn test_qualified_function_reference_typescript_callee_returns_no_function_reference() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "import * as ns from './lib';\nexport function run(): number {\n  return ns.f(1);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_typescript_parameter_qualifier_returns_no_edge() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "import * as ns from './lib';\nexport function run(obj: any, items: number[]): number[] {\n  return items.map(obj.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_typescript_variable_qualifier_keeps_value_read() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "import * as ns from './lib';\nconst LIMITS: any = {};\nexport function run(items: number[]): number[] {\n  return items.map(LIMITS.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
    assert!(
        value_reads(&g, "run")
            .iter()
            .any(|(_, name, ..)| name == "LIMITS"),
        "{:?}",
        value_reads(&g, "run")
    );
}

#[test]
fn test_qualified_function_reference_typescript_same_member_in_two_modules_binds_named_one() {
    let g = ts(&[
        ("a.ts", "export function run(x: number): number {\n  return x;\n}\n"),
        ("b.ts", "export function run(x: number): number {\n  return x;\n}\n"),
        (
            "app.ts",
            "import * as a from './a';\nimport * as b from './b';\nexport function go(items: number[]): number[] {\n  return items.map(a.run);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "a.ts", "run");
}

#[test]
fn test_qualified_function_reference_typescript_unresolvable_qualifier_returns_no_edge() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "export function run(items: number[]): number[] {\n  return items.map(missing.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_typescript_this_and_chain_qualifier_return_no_edge() {
    let g = ts(&[
        TS_LIB,
        (
            "app.ts",
            "import * as ns from './lib';\nexport class C {\n  f(x: number): number {\n    return x;\n  }\n  run(items: number[]): number[] {\n    return items.map(this.f).map(ns.sub.f);\n  }\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- JavaScript: `ns.f` after `import * as ns`, `Util.f` ----

const JS_LIB: (&str, &str) = ("lib.js", "export function f(x) {\n  return x;\n}\n");

#[test]
fn test_qualified_function_reference_javascript_namespace_argument_binds_member() {
    let g = js(&[
        JS_LIB,
        (
            "app.js",
            "import * as ns from './lib.js';\nexport function run(items) {\n  return items.map(ns.f);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "run"), "lib.js", "f");
}

#[test]
fn test_qualified_function_reference_javascript_class_static_argument_binds_method() {
    let g = js(&[(
        "app.js",
        "class Util {\n  static f(x) {\n    return x;\n  }\n}\nexport function run(items) {\n  return items.map(Util.f);\n}\n",
    )]);
    let edges = function_refs(&g, "run");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(
        (edges[0].0.as_str(), edges[0].1.as_str()),
        ("app.js", "f"),
        "{edges:?}"
    );
}

#[test]
fn test_qualified_function_reference_javascript_callee_returns_no_function_reference() {
    let g = js(&[
        JS_LIB,
        (
            "app.js",
            "import * as ns from './lib.js';\nexport function run() {\n  return ns.f(1);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_javascript_parameter_qualifier_returns_no_edge() {
    let g = js(&[
        JS_LIB,
        (
            "app.js",
            "import * as ns from './lib.js';\nexport function run(obj, items) {\n  return items.map(obj.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_qualified_function_reference_javascript_variable_qualifier_keeps_value_read() {
    let g = js(&[
        JS_LIB,
        (
            "app.js",
            "import * as ns from './lib.js';\nconst LIMITS = {};\nexport function run(items) {\n  return items.map(LIMITS.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
    assert!(
        value_reads(&g, "run")
            .iter()
            .any(|(_, name, ..)| name == "LIMITS"),
        "{:?}",
        value_reads(&g, "run")
    );
}

#[test]
fn test_qualified_function_reference_javascript_same_member_in_two_modules_binds_named_one() {
    let g = js(&[
        ("a.js", "export function run(x) {\n  return x;\n}\n"),
        ("b.js", "export function run(x) {\n  return x;\n}\n"),
        (
            "app.js",
            "import * as a from './a.js';\nimport * as b from './b.js';\nexport function go(items) {\n  return items.map(a.run);\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "go"), "a.js", "run");
}

#[test]
fn test_qualified_function_reference_javascript_unresolvable_qualifier_returns_no_edge() {
    let g = js(&[
        JS_LIB,
        (
            "app.js",
            "export function run(items) {\n  return items.map(missing.f);\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- Go: `pkg.F` after `import "example.com/app/pkg"` ----
// Go has no class-static syntax: a type's function value is a method
// expression (`T.M`), which no package qualifier covers.

#[test]
fn test_qualified_function_reference_go_package_argument_binds_member() {
    let g = go_module(&[
        GO_MOD,
        ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(pkg.F)\n}\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "boot"), "pkg/helper.go", "F");
}

#[test]
fn test_qualified_function_reference_go_callee_returns_no_function_reference() {
    let g = go_module(&[
        GO_MOD,
        ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc boot() {\n\tpkg.F()\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "boot"), none());
}

#[test]
fn test_qualified_function_reference_go_parameter_qualifier_returns_no_edge() {
    let g = go_module(&[
        GO_MOD,
        ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot(obj struct{ F func() }) {\n\tregister(obj.F)\n}\n\nvar _ = pkg.F\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "boot"), none());
}

#[test]
fn test_qualified_function_reference_go_variable_qualifier_keeps_value_read() {
    let g = go_module(&[
        GO_MOD,
        ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\ntype holder struct{ F func() }\n\nvar limits = holder{}\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(limits.F)\n}\n\nvar _ = pkg.F\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "boot"), none());
    assert!(
        value_reads(&g, "boot")
            .iter()
            .any(|(_, name, ..)| name == "limits"),
        "{:?}",
        value_reads(&g, "boot")
    );
}

#[test]
fn test_qualified_function_reference_go_same_member_in_two_packages_binds_named_one() {
    let g = go_module(&[
        GO_MOD,
        ("a/helper.go", "package a\n\nfunc F() {}\n"),
        ("b/helper.go", "package b\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nimport (\n\t\"example.com/app/a\"\n\t\"example.com/app/b\"\n)\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(a.F)\n}\n\nvar _ = b.F\n",
        ),
    ]);
    assert_one_cross_file(&function_refs(&g, "boot"), "a/helper.go", "F");
}

#[test]
fn test_qualified_function_reference_go_unresolvable_qualifier_returns_no_edge() {
    let g = go_module(&[
        GO_MOD,
        ("pkg/helper.go", "package pkg\n\nfunc F() {}\n"),
        (
            "cmd/app.go",
            "package main\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(missing.F)\n}\n",
        ),
    ]);
    assert_eq!(function_refs(&g, "boot"), none());
}
