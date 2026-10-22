//! FU-2026-10-22-593f351f77cf: a function named as a value, not called
//! (`addEventListener('x', handler)`, `Thread(target=worker)`,
//! `xs.map(::transform)`, `qsort(.., cmp)`), gets a `References` edge with
//! reason `function_reference` from the registering function, so `ecp
//! impact` no longer calls it dead. The edge is never a `Calls` edge: the
//! registrar hands the function on, it does not invoke it.
//!
//! Per language: the bare name (and the method-reference form where the
//! language has one) binds; a parameter of the same name binds nothing; a
//! direct call still yields exactly its `Calls` edge and no reference.
//!
//! PHP names a callable by string (`array_map('fmt', ..)`), which belongs to
//! the `call_user_func` literal path, not here: a bare PHP name is a
//! constant.
mod name_ref_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::RelType;
use name_ref_support::{
    assert_one_cross_file, calls, function_refs, graph, r, Ref, FUNCTION_REFERENCE, IMPORT,
    SAME_FILE,
};

fn none() -> Vec<Ref> {
    Vec::new()
}

/// `registrar` in `src` references `target` of `file` at the same-file tier.
fn assert_same_file_ref(
    provider: &dyn LanguageProvider,
    file: &str,
    src: &str,
    registrar: &str,
    target: &str,
) {
    let g = graph(provider, &[(file, src)]);
    assert_eq!(
        function_refs(&g, registrar),
        vec![r(file, target, FUNCTION_REFERENCE, SAME_FILE)],
        "{file}: {src}"
    );
}

/// `registrar` in `src` references nothing.
fn assert_no_ref(provider: &dyn LanguageProvider, file: &str, src: &str, registrar: &str) {
    let g = graph(provider, &[(file, src)]);
    assert_eq!(function_refs(&g, registrar), none(), "{file}: {src}");
}

/// `caller` in `src` calls `target` and references nothing.
fn assert_call_only(
    provider: &dyn LanguageProvider,
    file: &str,
    src: &str,
    caller: &str,
    target: &str,
) {
    let g = graph(provider, &[(file, src)]);
    assert_eq!(
        calls(&g, caller),
        vec![(file.to_string(), target.to_string())],
        "{file}: {src}"
    );
    assert_eq!(function_refs(&g, caller), none(), "{file}: {src}");
}

// ---- TypeScript ----

#[test]
fn test_function_reference_typescript_event_listener_argument_returns_edge() {
    assert_same_file_ref(
        &TypeScriptProvider::new().unwrap(),
        "app.ts",
        "function handler(): void {}\nexport function wire(el: any): void {\n  el.addEventListener('click', handler);\n}\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_typescript_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &TypeScriptProvider::new().unwrap(),
        "app.ts",
        "function handler(): void {}\nexport function wire(handler: () => void): void {\n  setTimeout(handler);\n}\n",
        "wire",
    );
}

#[test]
fn test_function_reference_typescript_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &TypeScriptProvider::new().unwrap(),
        "app.ts",
        "function handler(): void {}\nexport function run(): void {\n  handler();\n}\n",
        "run",
        "handler",
    );
}

#[test]
fn test_function_reference_typescript_imported_map_argument_returns_import_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            (
                "lib.ts",
                "export function transform(x: number): number {\n  return x;\n}\n",
            ),
            (
                "app.ts",
                "import { transform } from './lib';\nexport function apply(xs: number[]): number[] {\n  return xs.map(transform);\n}\n",
            ),
        ],
    );
    assert_eq!(
        function_refs(&g, "apply"),
        vec![r("lib.ts", "transform", FUNCTION_REFERENCE, IMPORT)]
    );
}

// ---- JavaScript ----

#[test]
fn test_function_reference_javascript_set_timeout_argument_returns_edge() {
    assert_same_file_ref(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "function tick() {}\nexport function start() {\n  setTimeout(tick, 10);\n}\n",
        "start",
        "tick",
    );
}

#[test]
fn test_function_reference_javascript_stored_handler_returns_edge() {
    assert_same_file_ref(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "function onClick() {}\nexport function wire(el) {\n  el.onclick = onClick;\n}\n",
        "wire",
        "onClick",
    );
}

#[test]
fn test_function_reference_javascript_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "function tick() {}\nexport function start(tick) {\n  setTimeout(tick, 10);\n}\n",
        "start",
    );
}

#[test]
fn test_function_reference_javascript_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "function tick() {}\nexport function start() {\n  tick();\n}\n",
        "start",
        "tick",
    );
}

// ---- Python ----

#[test]
fn test_function_reference_python_thread_target_keyword_returns_edge() {
    assert_same_file_ref(
        &PythonProvider::new().unwrap(),
        "app.py",
        "import threading\n\ndef worker():\n    pass\n\ndef start():\n    threading.Thread(target=worker).start()\n",
        "start",
        "worker",
    );
}

#[test]
fn test_function_reference_python_map_argument_returns_edge() {
    assert_same_file_ref(
        &PythonProvider::new().unwrap(),
        "app.py",
        "def transform(x):\n    return x\n\ndef apply(xs):\n    return list(map(transform, xs))\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_python_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &PythonProvider::new().unwrap(),
        "app.py",
        "def worker():\n    pass\n\ndef start(worker):\n    run(worker)\n",
        "start",
    );
}

#[test]
fn test_function_reference_python_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &PythonProvider::new().unwrap(),
        "app.py",
        "def worker():\n    pass\n\ndef start():\n    worker()\n",
        "start",
        "worker",
    );
}

// ---- Java ----

#[test]
fn test_function_reference_java_this_method_reference_returns_edge() {
    assert_same_file_ref(
        &JavaProvider::new().unwrap(),
        "P.java",
        "class P {\n    void print(String s) {}\n    void run(java.util.List<String> xs) {\n        xs.forEach(this::print);\n    }\n}\n",
        "run",
        "print",
    );
}

#[test]
fn test_function_reference_java_type_method_reference_returns_edge() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[(
            "Q.java",
            "class Fmt {\n    static String render(Object o) { return \"\"; }\n}\nclass Q {\n    void run(java.util.List<Object> xs) {\n        xs.stream().map(Fmt::render);\n    }\n}\n",
        )],
    );
    assert_one_cross_file(&function_refs(&g, "run"), "Q.java", "render");
}

/// A bare Java name is a variable, never a method: the parameter `print`
/// binds nothing.
#[test]
fn test_function_reference_java_parameter_named_like_method_returns_no_edge() {
    assert_no_ref(
        &JavaProvider::new().unwrap(),
        "P.java",
        "class P {\n    void print(String s) {}\n    void run(Runnable print) {\n        exec(print);\n    }\n    void exec(Runnable r) {}\n}\n",
        "run",
    );
}

#[test]
fn test_function_reference_java_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &JavaProvider::new().unwrap(),
        "P.java",
        "class P {\n    void print(String s) {}\n    void run() {\n        print(\"x\");\n    }\n}\n",
        "run",
        "print",
    );
}

// ---- Kotlin ----

#[test]
fn test_function_reference_kotlin_callable_reference_returns_edge() {
    assert_same_file_ref(
        &KotlinProvider::new().unwrap(),
        "k.kt",
        "fun transform(x: Int): Int = x\n\nfun apply(xs: List<Int>): List<Int> {\n    return xs.map(::transform)\n}\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_kotlin_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &KotlinProvider::new().unwrap(),
        "k.kt",
        "fun transform(x: Int): Int = x\n\nfun apply(xs: List<Int>, transform: (Int) -> Int): List<Int> {\n    return xs.map(transform)\n}\n",
        "apply",
    );
}

#[test]
fn test_function_reference_kotlin_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &KotlinProvider::new().unwrap(),
        "k.kt",
        "fun transform(x: Int): Int = x\n\nfun one(): Int {\n    return transform(1)\n}\n",
        "one",
        "transform",
    );
}

// ---- C# ----

#[test]
fn test_function_reference_csharp_method_group_argument_returns_edge() {
    assert_same_file_ref(
        &CSharpProvider::new().unwrap(),
        "P.cs",
        "class P {\n    void Print(string s) {}\n    void Run(System.Collections.Generic.List<string> xs) {\n        xs.ForEach(Print);\n    }\n}\n",
        "Run",
        "Print",
    );
}

#[test]
fn test_function_reference_csharp_parameter_shadows_method_returns_no_edge() {
    assert_no_ref(
        &CSharpProvider::new().unwrap(),
        "P.cs",
        "class P {\n    void Print(string s) {}\n    void Run(System.Action<string> Print) {\n        Exec(Print);\n    }\n    void Exec(System.Action<string> a) {}\n}\n",
        "Run",
    );
}

#[test]
fn test_function_reference_csharp_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &CSharpProvider::new().unwrap(),
        "P.cs",
        "class P {\n    void Print(string s) {}\n    void Run() {\n        Print(\"x\");\n    }\n}\n",
        "Run",
        "Print",
    );
}

// ---- Go ----

#[test]
fn test_function_reference_go_handle_func_argument_returns_edge() {
    assert_same_file_ref(
        &GoProvider::new().unwrap(),
        "main.go",
        "package main\n\nimport \"net/http\"\n\nfunc handler(w http.ResponseWriter, r *http.Request) {}\n\nfunc wire() {\n\thttp.HandleFunc(\"/\", handler)\n}\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_go_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &GoProvider::new().unwrap(),
        "main.go",
        "package main\n\nfunc handler() {}\n\nfunc register(f func()) {}\n\nfunc wire(handler func()) {\n\tregister(handler)\n}\n",
        "wire",
    );
}

#[test]
fn test_function_reference_go_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &GoProvider::new().unwrap(),
        "main.go",
        "package main\n\nfunc handler() {}\n\nfunc run() {\n\thandler()\n}\n",
        "run",
        "handler",
    );
}

#[test]
fn test_function_reference_go_same_package_other_file_returns_cross_file_edge() {
    let g = graph(
        &GoProvider::new().unwrap(),
        &[
            ("a.go", "package main\n\nfunc handler() {}\n"),
            (
                "b.go",
                "package main\n\nfunc register(f func()) {}\n\nfunc wire() {\n\tregister(handler)\n}\n",
            ),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "wire"), "a.go", "handler");
}

// ---- Rust ----

#[test]
fn test_function_reference_rust_map_argument_returns_edge() {
    assert_same_file_ref(
        &RustProvider::new().unwrap(),
        "src/lib.rs",
        "fn parse_line(s: &str) -> u32 {\n    s.len() as u32\n}\n\npub fn parse_all(lines: &[&str]) -> Vec<u32> {\n    lines.iter().copied().map(parse_line).collect()\n}\n",
        "parse_all",
        "parse_line",
    );
}

#[test]
fn test_function_reference_rust_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &RustProvider::new().unwrap(),
        "src/lib.rs",
        "fn parse_line(s: &str) -> u32 {\n    s.len() as u32\n}\n\npub fn parse_all(lines: &[&str], parse_line: fn(&str) -> u32) -> Vec<u32> {\n    lines.iter().copied().map(parse_line).collect()\n}\n",
        "parse_all",
    );
}

#[test]
fn test_function_reference_rust_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &RustProvider::new().unwrap(),
        "src/lib.rs",
        "fn parse_line(s: &str) -> u32 {\n    s.len() as u32\n}\n\npub fn one() -> u32 {\n    parse_line(\"x\")\n}\n",
        "one",
        "parse_line",
    );
}

// ---- PHP ----

/// A builtin that takes a callable takes PHP's function value, a string.
#[test]
fn test_function_reference_php_string_callable_returns_edge() {
    assert_same_file_ref(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($xs) {\n    return array_map('fmt', $xs);\n}\n",
        "apply",
        "fmt",
    );
}

#[test]
fn test_function_reference_php_static_method_string_callable_returns_edge() {
    let g = graph(
        &PhpProvider::new().unwrap(),
        &[(
            "p.php",
            "<?php\nclass Sorter {\n    public static function byName($a, $b) { return 0; }\n}\n\nfunction sortRows($rows) {\n    usort($rows, 'Sorter::byName');\n    return $rows;\n}\n",
        )],
    );
    // The qualifier-scoped tier, as a direct `Sorter::byName()` call binds.
    assert_eq!(
        function_refs(&g, "sortRows"),
        vec![r("p.php", "byName", FUNCTION_REFERENCE, 0.85)]
    );
}

/// The string is a callable only in the builtin's callable position.
#[test]
fn test_function_reference_php_string_outside_callable_position_returns_no_edge() {
    assert_no_ref(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($xs) {\n    return array_filter(['fmt'], 'is_string') + [str_repeat('fmt', 2)];\n}\n",
        "apply",
    );
}

/// A bare PHP name is a constant: it never binds the same-named function.
#[test]
fn test_function_reference_php_bare_name_never_binds_function_returns_no_edge() {
    assert_no_ref(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($xs) {\n    return array_map(fmt, $xs);\n}\n",
        "apply",
    );
}

#[test]
fn test_function_reference_php_variable_callable_returns_no_edge() {
    assert_no_ref(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction apply($fmt) {\n    return array_map($fmt, [1]);\n}\n",
        "apply",
    );
}

#[test]
fn test_function_reference_php_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &PhpProvider::new().unwrap(),
        "p.php",
        "<?php\nfunction fmt($x) { return $x; }\n\nfunction run() {\n    return fmt(1);\n}\n",
        "run",
        "fmt",
    );
}

// ---- Ruby ----

#[test]
fn test_function_reference_ruby_method_symbol_returns_edge() {
    assert_same_file_ref(
        &RubyProvider::new().unwrap(),
        "svc.rb",
        "class Svc\n  def handler(x)\n    x\n  end\n\n  def wire(items)\n    items.each(&method(:handler))\n  end\nend\n",
        "wire",
        "handler",
    );
}

#[test]
fn test_function_reference_ruby_parameter_named_like_method_returns_no_edge() {
    assert_no_ref(
        &RubyProvider::new().unwrap(),
        "svc.rb",
        "class Svc\n  def handler(x)\n    x\n  end\n\n  def wire(handler)\n    handler.call(1)\n  end\nend\n",
        "wire",
    );
}

#[test]
fn test_function_reference_ruby_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &RubyProvider::new().unwrap(),
        "svc.rb",
        "class Svc\n  def handler(x)\n    x\n  end\n\n  def run\n    handler(1)\n  end\nend\n",
        "run",
        "handler",
    );
}

// ---- Swift ----

#[test]
fn test_function_reference_swift_map_argument_returns_edge() {
    assert_same_file_ref(
        &SwiftProvider::new().unwrap(),
        "s.swift",
        "func transform(_ x: Int) -> Int {\n    return x\n}\n\nfunc apply(_ xs: [Int]) -> [Int] {\n    return xs.map(transform)\n}\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_swift_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &SwiftProvider::new().unwrap(),
        "s.swift",
        "func transform(_ x: Int) -> Int {\n    return x\n}\n\nfunc apply(_ xs: [Int], transform: (Int) -> Int) -> [Int] {\n    return xs.map(transform)\n}\n",
        "apply",
    );
}

#[test]
fn test_function_reference_swift_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &SwiftProvider::new().unwrap(),
        "s.swift",
        "func transform(_ x: Int) -> Int {\n    return x\n}\n\nfunc one() -> Int {\n    return transform(1)\n}\n",
        "one",
        "transform",
    );
}

// ---- C ----

#[test]
fn test_function_reference_c_qsort_comparator_returns_edge() {
    assert_same_file_ref(
        &CProvider::new().unwrap(),
        "s.c",
        "#include <stdlib.h>\n\nint cmp(const void *a, const void *b) {\n    return 0;\n}\n\nvoid sort_all(int *xs, int n) {\n    qsort(xs, n, sizeof(int), cmp);\n}\n",
        "sort_all",
        "cmp",
    );
}

#[test]
fn test_function_reference_c_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &CProvider::new().unwrap(),
        "s.c",
        "#include <stdlib.h>\n\nint cmp(const void *a, const void *b) {\n    return 0;\n}\n\nvoid sort_all(int *xs, int n, int (*cmp)(const void *, const void *)) {\n    qsort(xs, n, sizeof(int), cmp);\n}\n",
        "sort_all",
    );
}

#[test]
fn test_function_reference_c_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &CProvider::new().unwrap(),
        "s.c",
        "int cmp(const void *a, const void *b) {\n    return 0;\n}\n\nint one(void) {\n    return cmp(0, 0);\n}\n",
        "one",
        "cmp",
    );
}

// ---- C++ ----

#[test]
fn test_function_reference_cpp_sort_comparator_returns_edge() {
    assert_same_file_ref(
        &CppProvider::new().unwrap(),
        "s.cpp",
        "#include <algorithm>\n#include <vector>\n\nbool byValue(int a, int b) {\n    return a < b;\n}\n\nvoid sortAll(std::vector<int>& xs) {\n    std::sort(xs.begin(), xs.end(), byValue);\n}\n",
        "sortAll",
        "byValue",
    );
}

#[test]
fn test_function_reference_cpp_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &CppProvider::new().unwrap(),
        "s.cpp",
        "#include <algorithm>\n#include <vector>\n\nbool byValue(int a, int b) {\n    return a < b;\n}\n\nvoid sortAll(std::vector<int>& xs, bool (*byValue)(int, int)) {\n    std::sort(xs.begin(), xs.end(), byValue);\n}\n",
        "sortAll",
    );
}

#[test]
fn test_function_reference_cpp_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &CppProvider::new().unwrap(),
        "s.cpp",
        "bool byValue(int a, int b) {\n    return a < b;\n}\n\nbool one() {\n    return byValue(1, 2);\n}\n",
        "one",
        "byValue",
    );
}

// ---- Dart ----

#[test]
fn test_function_reference_dart_map_argument_returns_edge() {
    assert_same_file_ref(
        &DartProvider::new().unwrap(),
        "d.dart",
        "int transform(int x) {\n  return x;\n}\n\nList<int> apply(List<int> xs) {\n  return xs.map(transform).toList();\n}\n",
        "apply",
        "transform",
    );
}

#[test]
fn test_function_reference_dart_parameter_shadows_function_returns_no_edge() {
    assert_no_ref(
        &DartProvider::new().unwrap(),
        "d.dart",
        "int transform(int x) {\n  return x;\n}\n\nList<int> apply(List<int> xs, int Function(int) transform) {\n  return xs.map(transform).toList();\n}\n",
        "apply",
    );
}

#[test]
fn test_function_reference_dart_direct_call_returns_only_calls_edge() {
    assert_call_only(
        &DartProvider::new().unwrap(),
        "d.dart",
        "int transform(int x) {\n  return x;\n}\n\nint one() {\n  return transform(1);\n}\n",
        "one",
        "transform",
    );
}

/// The name scan stops at a method reference; the field-read walk does not.
#[test]
fn test_field_read_java_method_reference_receiver_field_returns_edge() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[(
            "J.java",
            "import java.util.List;\nclass Sink { void accept(String s) {} }\nclass J {\n    Sink sink;\n    void viaRef(List<String> xs) {\n        xs.forEach(this.sink::accept);\n    }\n}\n",
        )],
    );
    let reads: Vec<String> =
        name_ref_support::refs(&g, "viaRef", RelType::ReadsField, "field_read")
            .into_iter()
            .map(|(_, name, _, _)| name)
            .collect();
    assert_eq!(reads, vec!["sink".to_string()]);
}

/// A function declared inside the registrar is still a function it hands on.
#[test]
fn test_function_reference_typescript_nested_function_argument_returns_edge() {
    let src = "export function inner(r: number[]) {\n  function update(v: number) { return v; }\n  return r.map(update);\n}\n";
    assert_same_file_ref(
        &TypeScriptProvider::new().unwrap(),
        "a.ts",
        src,
        "inner",
        "update",
    );
}
