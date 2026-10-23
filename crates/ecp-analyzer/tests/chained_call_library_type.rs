//! A chained call (`fetchIt().json()`) types its receiver by the inner
//! callee's return type, read in the callee's own file. A return type that
//! the callee imports from a library names no project type, even when some
//! project file declares a class of that name: before the fix, the chain
//! typed it with no imports, so the global lookup bound the namesake.
//!
//! The callee file's imports type the name in Python, TS, JS and Java, so a
//! type imported from a project file still binds. PHP retains its
//! directory-scoped lookup. The overlay uses the same visibility rules.
//!
//! Each fixture declares the member on two more project classes, so the
//! untyped fallback binds nothing.
mod instantiation_calls_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::ZeroCopyGraph;
use instantiation_calls_support::calls_from;
use std::path::Path;

fn single(provider: &dyn LanguageProvider, files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        let local = provider
            .parse_file(Path::new(path), src.as_bytes())
            .unwrap_or_else(|e| panic!("parse {path}: {e}"));
        builder.add_graph(local);
    }
    builder.build()
}

/// Owners of the `Calls` targets named `member` from `caller`, sorted.
fn owners(graph: &ZeroCopyGraph, caller: &str, member: &str) -> Vec<String> {
    let mut out: Vec<String> = calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.name == member)
        .map(|hit| hit.owner)
        .collect();
    out.sort();
    out
}

fn assert_no_member_edge(graph: &ZeroCopyGraph, caller: &str, member: &str) {
    assert_eq!(
        owners(graph, caller, member),
        Vec::<String>::new(),
        "{caller}: the library return type binds no project `{member}`"
    );
}

fn assert_binds(graph: &ZeroCopyGraph, caller: &str, member: &str, owner: &str) {
    assert_eq!(
        owners(graph, caller, member),
        [owner],
        "{caller}: the project return type picks `{owner}.{member}`"
    );
}

// ---- TypeScript ----

fn ts_graph() -> ZeroCopyGraph {
    single(
        &TypeScriptProvider::new().unwrap(),
        &[
            (
                "src/http/response.ts",
                "export class Response {\n  json(): void {}\n}\n",
            ),
            ("src/alpha.ts", "export class Alpha {\n  json(): void {}\n}\n"),
            ("src/beta.ts", "export class Beta {\n  json(): void {}\n}\n"),
            (
                "src/client.ts",
                "import { Response } from \"node-fetch\";\n\nexport function fetchIt(): Response {\n  return fetch();\n}\n",
            ),
            (
                "src/own.ts",
                "import { Response } from \"./http/response\";\n\nexport function fetchOwn(): Response {\n  return new Response();\n}\n",
            ),
            (
                "src/use.ts",
                "import { fetchIt } from \"./client\";\nimport { fetchOwn } from \"./own\";\n\nexport function useLib() {\n  fetchIt().json();\n}\nexport function useOwn() {\n  fetchOwn().json();\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_chained_member_typescript_library_return_type_binds_no_namesake() {
    assert_no_member_edge(&ts_graph(), "useLib", "json");
}

#[test]
fn test_resolve_chained_member_typescript_project_imported_return_type_binds_type_member() {
    assert_binds(&ts_graph(), "useOwn", "json", "Response");
}

// ---- Python ----

fn py_graph() -> ZeroCopyGraph {
    single(
        &PythonProvider::new().unwrap(),
        &[
            (
                "web/response.py",
                "class Response:\n    def json(self):\n        pass\n",
            ),
            ("alpha.py", "class Alpha:\n    def json(self):\n        pass\n"),
            ("beta.py", "class Beta:\n    def json(self):\n        pass\n"),
            (
                "client.py",
                "from requests import Response\n\n\ndef fetch_it() -> Response:\n    return None\n",
            ),
            (
                "own.py",
                "from web.response import Response\n\n\ndef fetch_own() -> Response:\n    return Response()\n",
            ),
            (
                "use.py",
                "from client import fetch_it\nfrom own import fetch_own\n\n\ndef use_lib():\n    fetch_it().json()\n\n\ndef use_own():\n    fetch_own().json()\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_chained_member_python_library_return_type_binds_no_namesake() {
    assert_no_member_edge(&py_graph(), "use_lib", "json");
}

#[test]
fn test_resolve_chained_member_python_project_imported_return_type_binds_type_member() {
    assert_binds(&py_graph(), "use_own", "json", "Response");
}

// ---- PHP ----

fn php_graph() -> ZeroCopyGraph {
    single(
        &PhpProvider::new().unwrap(),
        &[
            (
                "src/Web/Response.php",
                "<?php\nclass Response {\n    public function body() {}\n}\n",
            ),
            (
                "src/Alpha.php",
                "<?php\nclass Alpha {\n    public function body() {}\n}\n",
            ),
            (
                "src/Beta.php",
                "<?php\nclass Beta {\n    public function body() {}\n}\n",
            ),
            (
                "src/Client.php",
                "<?php\nuse GuzzleHttp\\Psr7\\Response;\n\nclass Client {\n    public static function fetch(): Response { return new Response(); }\n}\n",
            ),
            (
                "src/Local.php",
                "<?php\nclass Local {\n    public static function make(): Local { return new Local(); }\n    public function body() {}\n}\n",
            ),
            (
                "src/use.php",
                "<?php\nfunction useLib() { Client::fetch()->body(); }\nfunction useOwn() { Local::make()->body(); }\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_chained_member_php_library_return_type_binds_no_namesake() {
    assert_no_member_edge(&php_graph(), "useLib", "body");
}

/// PHP binds only a return type the callee's own directory declares.
#[test]
fn test_resolve_chained_member_php_same_file_return_type_binds_type_member() {
    assert_binds(&php_graph(), "useOwn", "body", "Local");
}

// ---- Java ----

fn java_graph() -> ZeroCopyGraph {
    single(
        &JavaProvider::new().unwrap(),
        &[
            (
                "src/web/Response.java",
                "package web;\n\npublic class Response {\n    public void body() {}\n}\n",
            ),
            (
                "src/app/Alpha.java",
                "package app;\n\npublic class Alpha {\n    public void body() {}\n}\n",
            ),
            (
                "src/app/Beta.java",
                "package app;\n\npublic class Beta {\n    public void body() {}\n}\n",
            ),
            (
                "src/app/Client.java",
                "package app;\n\nimport okhttp3.Response;\n\npublic class Client {\n    public static Response fetch() { return null; }\n}\n",
            ),
            (
                "src/app/Imported.java",
                "package app;\n\nimport web.Response;\n\npublic class Imported {\n    public static Response fetch() { return new Response(); }\n}\n",
            ),
            (
                "src/app/Local.java",
                "package app;\n\npublic class Local {\n    public static Local make() { return new Local(); }\n    public void body() {}\n}\n",
            ),
            (
                "src/app/Use.java",
                "package app;\n\npublic class Use {\n    void useLib() { Client.fetch().body(); }\n    void useImported() { Imported.fetch().body(); }\n    void useOwn() { Local.make().body(); }\n}\n",
            ),
        ],
    )
}

#[test]
fn test_resolve_chained_member_java_library_return_type_binds_no_namesake() {
    assert_no_member_edge(&java_graph(), "useLib", "body");
}

/// A return type declared beside the callee needs no import.
#[test]
fn test_resolve_chained_member_java_same_file_return_type_binds_type_member() {
    assert_binds(&java_graph(), "useOwn", "body", "Local");
}

/// A project import binds the return type across package directories.
#[test]
fn test_resolve_chained_member_java_return_type_from_other_package_binds_member() {
    assert_binds(&java_graph(), "useImported", "body", "Response");
}
