//! A method handed on as a value through a receiver whose type the parser
//! knows (`xs.forEach(c::m)`, `apply(t.M)`, `map(self.m, xs)`,
//! `xs.forEach(this.m.bind(this))`) gets a `References` edge with reason
//! `function_reference` to the method of that type. The receiver is typed as
//! the call `c.m()` is: a parameter or local declared with the type, `this` /
//! `self`, a C# field, a Ruby call the receiver typing types. The type picks
//! the declaration: with `Cls.m` and `Other.m` in the tree, `c.m` binds only
//! `Cls`.
//!
//! Contract of every language: a receiver of no known type binds nothing
//! (no low-confidence guess either), a call `c.m(1)` keeps its `Calls` edge
//! and gains no `References`, and a field or property read (`c.count`) gets
//! no `function_reference` edge.
//!
//! A parenthesized callee `(c.m)(1)` is a call too (Go, Swift, Dart, C#,
//! Python, TypeScript, JavaScript). Java `c::m`, Kotlin `c::m` and Ruby
//! `c.method(:m)` have no parenthesized-callee form that parses to a method
//! value, so they carry no such test.
//!
//! Languages with no bound method value, so no positive test:
//! - C has no methods; `&f` is the only function value
//!   (`qualified_function_reference_types.rs`).
//! - C++ names a member function only as `&Cls::m`, which already binds
//!   through the qualified form (`qualified_function_reference_types.rs`).
//! - PHP's `[$o, 'm']`, `$o->m(...)` and callable strings are handled in
//!   `php_callable_reference.rs`.
//! - Rust `obj.m` without a call is a field access, never a method value; the
//!   guard test below keeps it edge-free.
//!
//! Receiver typing this file does not cover, as the call `c.m()` does not
//! type it either: a constructor-assigned untyped local (Python `o = Cls()`,
//! TypeScript / JavaScript `const o = new Cls()`, Ruby `o = Cls.new`,
//! Kotlin / Swift / Dart `val o = Cls()`) and a Go value of a type from
//! another package (`var t util.T`).
use crate::name_ref_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{assert_one_cross_file, calls, function_refs, graph, Ref};

fn none() -> Vec<Ref> {
    Vec::new()
}

fn refs(provider: &dyn LanguageProvider, files: &[(&str, &str)], source: &str) -> Vec<Ref> {
    function_refs(&graph(provider, files), source)
}

/// `source` calls `name` of `file` and hands on no function.
fn assert_call_only(g: &ZeroCopyGraph, source: &str, file: &str, name: &str) {
    assert_eq!(function_refs(g, source), none());
    let targets = calls(g, source);
    assert!(
        targets.contains(&(file.to_string(), name.to_string())),
        "the call keeps its Calls edge: {targets:?}"
    );
}

// ---- Java ----

const JAVA_CLS: &str = "class Cls {\n    int count;\n    void m(int x) {}\n    void selfRef(java.util.List<Integer> xs) {\n        xs.forEach(this::m);\n    }\n}\n";
const JAVA_OTHER: &str = "class Other {\n    void m(int x) {}\n}\n";

fn java_use(body: &str) -> String {
    format!(
        "class Use {{\n    void run(java.util.List<Integer> xs, Cls c, java.util.List<Cls> cs) {{\n        {body}\n    }}\n}}\n"
    )
}

fn java_refs(body: &str) -> Vec<Ref> {
    let used = java_use(body);
    refs(
        &JavaProvider::new().unwrap(),
        &[
            ("Cls.java", JAVA_CLS),
            ("Other.java", JAVA_OTHER),
            ("Use.java", used.as_str()),
        ],
        "run",
    )
}

#[test]
fn test_bound_method_reference_java_parameter_receiver_binds_its_type() {
    assert_one_cross_file(&java_refs("xs.forEach(c::m);"), "Cls.java", "m");
}

#[test]
fn test_bound_method_reference_java_local_receiver_binds_its_type() {
    let found = java_refs("Cls d = new Cls();\n        xs.forEach(d::m);");
    assert_one_cross_file(&found, "Cls.java", "m");
}

#[test]
fn test_bound_method_reference_java_this_receiver_binds_enclosing_class() {
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[("Cls.java", JAVA_CLS), ("Other.java", JAVA_OTHER)],
        "selfRef",
    );
    assert_one_cross_file(&found, "Cls.java", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_java_same_reference_twice_returns_one_edge() {
    let found = java_refs("xs.forEach(c::m);\n        xs.forEach(c::m);");
    assert_one_cross_file(&found, "Cls.java", "m");
}

#[test]
fn test_bound_method_reference_java_var_receiver_returns_no_edge() {
    assert_eq!(
        java_refs("var d = new Cls();\n        xs.forEach(d::m);"),
        none()
    );
}

/// A generic type (`List<Cls>`) is no plain class name: `cs` stays untyped.
#[test]
fn test_bound_method_reference_java_generic_receiver_returns_no_edge() {
    assert_eq!(java_refs("xs.forEach(cs::m);"), none());
}

#[test]
fn test_bound_method_reference_java_member_the_type_lacks_returns_no_edge() {
    assert_eq!(java_refs("xs.forEach(c::missing);"), none());
}

/// The receiver's type is not indexed: `Other.m` is never a fallback.
#[test]
fn test_bound_method_reference_java_type_not_indexed_returns_no_edge() {
    let used = java_use("xs.forEach(c::m);");
    let found = refs(
        &JavaProvider::new().unwrap(),
        &[("Other.java", JAVA_OTHER), ("Use.java", used.as_str())],
        "run",
    );
    assert_eq!(found, none());
}

#[test]
fn test_bound_method_reference_java_call_keeps_calls_edge_only() {
    let used = java_use("c.m(1);");
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("Cls.java", JAVA_CLS),
            ("Other.java", JAVA_OTHER),
            ("Use.java", used.as_str()),
        ],
    );
    assert_call_only(&g, "run", "Cls.java", "m");
}

#[test]
fn test_bound_method_reference_java_field_read_returns_no_edge() {
    let found = java_refs("int x = c.count;\n        System.out.println(c.count);");
    assert_eq!(found, none());
}

// ---- Kotlin ----

const KT_CLS: &str = "class Cls {\n    val count = 1\n    fun m(x: Int) {}\n    fun selfRef(xs: List<Int>) {\n        xs.forEach(this::m)\n    }\n}\n";
const KT_OTHER: &str = "class Other {\n    fun m(x: Int) {}\n}\n";

fn kotlin_graph(body: &str) -> ZeroCopyGraph {
    let used = format!("fun run(xs: List<Int>, c: Cls) {{\n    {body}\n}}\n");
    graph(
        &KotlinProvider::new().unwrap(),
        &[
            ("Cls.kt", KT_CLS),
            ("Other.kt", KT_OTHER),
            ("Use.kt", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_kotlin_parameter_receiver_binds_its_type() {
    let g = kotlin_graph("xs.forEach(c::m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.kt", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_kotlin_same_reference_twice_returns_one_edge() {
    let g = kotlin_graph("xs.forEach(c::m)\n    xs.forEach(c::m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.kt", "m");
}

#[test]
fn test_bound_method_reference_kotlin_typed_local_receiver_binds_its_type() {
    let g = kotlin_graph("val d: Cls = Cls()\n    xs.forEach(d::m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.kt", "m");
}

#[test]
fn test_bound_method_reference_kotlin_this_receiver_binds_enclosing_class() {
    let g = kotlin_graph("xs.forEach(c::m)");
    assert_one_cross_file(&function_refs(&g, "selfRef"), "Cls.kt", "m");
}

#[test]
fn test_bound_method_reference_kotlin_untyped_local_receiver_returns_no_edge() {
    let g = kotlin_graph("val d = Cls()\n    xs.forEach(d::m)");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_kotlin_call_keeps_calls_edge_only() {
    let g = kotlin_graph("c.m(1)");
    assert_call_only(&g, "run", "Cls.kt", "m");
}

#[test]
fn test_bound_method_reference_kotlin_property_read_returns_no_edge() {
    let g = kotlin_graph("val x = c.count\n    println(c.count + x)");
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- Go ----

const GO_TYPES: &str = "package main\n\ntype T struct{ Count int }\n\nfunc (t T) M(x int) {}\n\nfunc (t T) selfRef() { apply(t.M) }\n\nfunc apply(f func(int)) {}\n";
const GO_OTHER: &str = "package main\n\ntype Other struct{}\n\nfunc (o Other) M(x int) {}\n";

fn go_graph(functions: &str) -> ZeroCopyGraph {
    let used = format!("package main\n\n{functions}\n");
    graph(
        &GoProvider::new().unwrap(),
        &[
            ("types.go", GO_TYPES),
            ("other.go", GO_OTHER),
            ("main.go", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_go_var_receiver_binds_its_type() {
    let g = go_graph("func run() {\n\tvar t T\n\tapply(t.M)\n}");
    assert_one_cross_file(&function_refs(&g, "run"), "types.go", "M");
}

#[test]
fn test_bound_method_reference_go_parameter_receiver_binds_its_type() {
    let g = go_graph("func run(t *T) {\n\tapply(t.M)\n}");
    assert_one_cross_file(&function_refs(&g, "run"), "types.go", "M");
}

#[test]
fn test_bound_method_reference_go_composite_literal_receiver_binds_its_type() {
    let g = go_graph("func run() {\n\tt := T{}\n\tapply(t.M)\n}");
    assert_one_cross_file(&function_refs(&g, "run"), "types.go", "M");
}

#[test]
fn test_bound_method_reference_go_method_receiver_binds_its_type() {
    let g = go_graph("func run() {}");
    assert_one_cross_file(&function_refs(&g, "selfRef"), "types.go", "M");
}

#[test]
fn test_bound_method_reference_go_untyped_receiver_returns_no_edge() {
    let g = go_graph("func run(get func() T) {\n\tt := get()\n\tapply(t.M)\n}");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_go_call_keeps_calls_edge_only() {
    let g = go_graph("func run(t T) {\n\tt.M(1)\n}");
    assert_call_only(&g, "run", "types.go", "M");
}

/// Stands for: a parenthesized callee `(c.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `c.m(1)` records.
#[test]
fn test_bound_method_reference_go_parenthesized_call_keeps_calls_edge_only() {
    let g = go_graph("func run(t T) {\n\t(t.M)(1)\n}");
    assert_call_only(&g, "run", "types.go", "M");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_go_same_reference_twice_returns_one_edge() {
    let g = go_graph("func run(t T) {\n\tapply(t.M)\n\tapply(t.M)\n}");
    assert_one_cross_file(&function_refs(&g, "run"), "types.go", "M");
}

#[test]
fn test_bound_method_reference_go_field_read_returns_no_edge() {
    let g = go_graph("func run(t T) {\n\tx := t.Count\n\tprintln(x, t.Count)\n}");
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- Swift ----

const SWIFT_CLS: &str = "class Cls {\n    var count = 1\n    func m(_ x: Int) {}\n    func selfRef(_ xs: [Int]) {\n        xs.forEach(self.m)\n    }\n}\n";
const SWIFT_OTHER: &str = "class Other {\n    func m(_ x: Int) {}\n}\n";

fn swift_graph(body: &str) -> ZeroCopyGraph {
    let used = format!("func run(_ xs: [Int], _ c: Cls) {{\n    {body}\n}}\n");
    graph(
        &SwiftProvider::new().unwrap(),
        &[
            ("Cls.swift", SWIFT_CLS),
            ("Other.swift", SWIFT_OTHER),
            ("Use.swift", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_swift_parameter_receiver_binds_its_type() {
    let g = swift_graph("xs.forEach(c.m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.swift", "m");
}

#[test]
fn test_bound_method_reference_swift_typed_local_receiver_binds_its_type() {
    let g = swift_graph("let d: Cls = Cls()\n    xs.forEach(d.m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.swift", "m");
}

#[test]
fn test_bound_method_reference_swift_self_receiver_binds_enclosing_class() {
    let g = swift_graph("xs.forEach(c.m)");
    assert_one_cross_file(&function_refs(&g, "selfRef"), "Cls.swift", "m");
}

#[test]
fn test_bound_method_reference_swift_untyped_local_receiver_returns_no_edge() {
    let g = swift_graph("let d = Cls()\n    xs.forEach(d.m)");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_swift_call_keeps_calls_edge_only() {
    let g = swift_graph("c.m(1)");
    assert_call_only(&g, "run", "Cls.swift", "m");
}

/// Stands for: a parenthesized callee `(c.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `c.m(1)` records.
#[test]
fn test_bound_method_reference_swift_parenthesized_call_keeps_calls_edge_only() {
    let g = swift_graph("(c.m)(1)");
    assert_call_only(&g, "run", "Cls.swift", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_swift_same_reference_twice_returns_one_edge() {
    let g = swift_graph("xs.forEach(c.m)\n    xs.forEach(c.m)");
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.swift", "m");
}

#[test]
fn test_bound_method_reference_swift_property_read_returns_no_edge() {
    let g = swift_graph("let x = c.count\n    print(x, c.count)");
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- Dart ----

const DART_CLS: &str = "class Cls {\n  int count = 1;\n  void m(int x) {}\n  void selfRef(List<int> xs) {\n    xs.forEach(this.m);\n  }\n}\n";
const DART_OTHER: &str = "class Other {\n  void m(int x) {}\n}\n";

fn dart_graph(body: &str) -> ZeroCopyGraph {
    let used = format!(
        "import 'cls.dart';\nimport 'other.dart';\nvoid run(List<int> xs, Cls c) {{\n  {body}\n}}\n"
    );
    graph(
        &DartProvider::new().unwrap(),
        &[
            ("cls.dart", DART_CLS),
            ("other.dart", DART_OTHER),
            ("use.dart", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_dart_parameter_receiver_binds_its_type() {
    let g = dart_graph("xs.forEach(c.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.dart", "m");
}

#[test]
fn test_bound_method_reference_dart_typed_local_receiver_binds_its_type() {
    let g = dart_graph("Cls d = Cls();\n  xs.forEach(d.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.dart", "m");
}

#[test]
fn test_bound_method_reference_dart_this_receiver_binds_enclosing_class() {
    let g = dart_graph("xs.forEach(c.m);");
    assert_one_cross_file(&function_refs(&g, "selfRef"), "cls.dart", "m");
}

#[test]
fn test_bound_method_reference_dart_untyped_local_receiver_returns_no_edge() {
    let g = dart_graph("var d = Cls();\n  xs.forEach(d.m);");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_dart_call_keeps_calls_edge_only() {
    let g = dart_graph("c.m(1);");
    assert_call_only(&g, "run", "cls.dart", "m");
}

/// Stands for: a parenthesized callee `(c.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `c.m(1)` records.
#[test]
fn test_bound_method_reference_dart_parenthesized_call_keeps_calls_edge_only() {
    let g = dart_graph("(c.m)(1);");
    assert_call_only(&g, "run", "cls.dart", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_dart_same_reference_twice_returns_one_edge() {
    let g = dart_graph("xs.forEach(c.m);\n  xs.forEach(c.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.dart", "m");
}

#[test]
fn test_bound_method_reference_dart_field_read_returns_no_edge() {
    let g = dart_graph("var x = c.count;\n  print(c.count + x);");
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- C# ----

const CS_CLS: &str = "public class Cls {\n    public int Count;\n    public void M(int x) {}\n    public void SelfRef(System.Collections.Generic.List<int> xs) {\n        xs.ForEach(this.M);\n    }\n}\n";
const CS_OTHER: &str = "public class Other {\n    public void M(int x) {}\n}\n";

fn csharp_graph(body: &str) -> ZeroCopyGraph {
    let used = format!(
        "using System.Collections.Generic;\npublic class Use {{\n    Cls held;\n    public void Run(List<int> xs, Cls c) {{\n        {body}\n    }}\n}}\n"
    );
    graph(
        &CSharpProvider::new().unwrap(),
        &[
            ("Cls.cs", CS_CLS),
            ("Other.cs", CS_OTHER),
            ("Use.cs", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_csharp_parameter_receiver_binds_its_type() {
    let g = csharp_graph("xs.ForEach(c.M);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_var_new_local_receiver_binds_its_type() {
    let g = csharp_graph("var d = new Cls();\n        xs.ForEach(d.M);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_field_receiver_binds_its_type() {
    let g = csharp_graph("xs.ForEach(held.M);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_delegate_creation_argument_binds_its_type() {
    let g = csharp_graph("var a = new System.Action<int>(c.M);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_this_receiver_binds_enclosing_class() {
    let g = csharp_graph("xs.ForEach(c.M);");
    assert_one_cross_file(&function_refs(&g, "SelfRef"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_untyped_var_receiver_returns_no_edge() {
    let g = csharp_graph("var d = Pick();\n        xs.ForEach(d.M);");
    assert_eq!(function_refs(&g, "Run"), none());
}

#[test]
fn test_bound_method_reference_csharp_call_keeps_calls_edge_only() {
    let g = csharp_graph("c.M(1);");
    assert_call_only(&g, "Run", "Cls.cs", "M");
}

/// Stands for: a parenthesized callee `(c.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `c.m(1)` records.
#[test]
fn test_bound_method_reference_csharp_parenthesized_call_keeps_calls_edge_only() {
    let g = csharp_graph("(c.M)(1);");
    assert_call_only(&g, "Run", "Cls.cs", "M");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_csharp_same_reference_twice_returns_one_edge() {
    let g = csharp_graph("xs.ForEach(c.M);\n        xs.ForEach(c.M);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

#[test]
fn test_bound_method_reference_csharp_field_read_returns_no_edge() {
    let g = csharp_graph("int x = c.Count;\n        System.Console.WriteLine(c.Count + x);");
    assert_eq!(function_refs(&g, "Run"), none());
}

// ---- Python ----

const PY_CLS: &str = "class Cls:\n    count = 1\n\n    def m(self, x):\n        return x\n\n    def self_ref(self, xs):\n        return list(map(self.m, xs))\n";
const PY_OTHER: &str = "class Other:\n    def m(self, x):\n        return x\n";

fn python_graph(body: &str) -> ZeroCopyGraph {
    let used = format!(
        "from cls import Cls\nfrom other import Other\n\n\ndef run(xs, obj: Cls, loose):\n    {body}\n"
    );
    graph(
        &PythonProvider::new().unwrap(),
        &[
            ("cls.py", PY_CLS),
            ("other.py", PY_OTHER),
            ("use.py", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_python_annotated_parameter_receiver_binds_its_type() {
    let g = python_graph("return list(map(obj.m, xs))");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_annotated_local_receiver_binds_its_type() {
    let g = python_graph("local: Cls = Cls()\n    return list(map(local.m, xs))");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_self_receiver_binds_enclosing_class() {
    let g = python_graph("return xs");
    assert_one_cross_file(&function_refs(&g, "self_ref"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_untyped_parameter_receiver_returns_no_edge() {
    let g = python_graph("return list(map(loose.m, xs))");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_python_assignment_target_returns_no_edge() {
    let g = python_graph("obj.m = None");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_python_call_keeps_calls_edge_only() {
    let g = python_graph("obj.m(1)");
    assert_call_only(&g, "run", "cls.py", "m");
}

/// Stands for: a parenthesized callee `(obj.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `obj.m(1)` records.
#[test]
fn test_bound_method_reference_python_parenthesized_call_keeps_calls_edge_only() {
    let g = python_graph("(obj.m)(1)");
    assert_call_only(&g, "run", "cls.py", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_python_same_reference_twice_returns_one_edge() {
    let g = python_graph("list(map(obj.m, xs))\n    list(map(obj.m, xs))");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_attribute_read_returns_no_edge() {
    let g = python_graph("x = obj.count\n    print(obj.count, x)");
    assert_eq!(function_refs(&g, "run"), none());
}

// ---- TypeScript ----

const TS_CLS: &str = "export class Cls {\n  count = 1;\n  m(x: number): void {}\n  selfRef(xs: number[]): void {\n    xs.forEach(this.m);\n  }\n  selfBind(xs: number[]): void {\n    xs.forEach(this.m.bind(this));\n  }\n  readCount(): number {\n    return this.count;\n  }\n}\n";
const TS_OTHER: &str = "export class Other {\n  m(x: number): void {}\n}\n";

fn ts_graph(body: &str) -> ZeroCopyGraph {
    let used = format!(
        "import {{ Cls }} from './cls';\nimport {{ Other }} from './other';\nexport function run(xs: number[], obj: Cls, loose: any): void {{\n  {body}\n}}\n"
    );
    graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("cls.ts", TS_CLS),
            ("other.ts", TS_OTHER),
            ("use.ts", used.as_str()),
        ],
    )
}

#[test]
fn test_bound_method_reference_typescript_annotated_parameter_receiver_binds_its_type() {
    let g = ts_graph("xs.forEach(obj.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_annotated_local_receiver_binds_its_type() {
    let g = ts_graph("const local: Cls = new Cls();\n  xs.forEach(local.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_this_receiver_binds_enclosing_class() {
    let g = ts_graph("return;");
    assert_one_cross_file(&function_refs(&g, "selfRef"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_bind_this_binds_enclosing_class() {
    let g = ts_graph("return;");
    assert_one_cross_file(&function_refs(&g, "selfBind"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_untyped_receiver_returns_no_edge() {
    let g = ts_graph("xs.forEach(loose.m);");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_typescript_this_field_read_returns_no_edge() {
    let g = ts_graph("return;");
    assert_eq!(function_refs(&g, "readCount"), none());
}

#[test]
fn test_bound_method_reference_typescript_assignment_target_returns_no_edge() {
    let g = ts_graph("obj.m = loose;");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_typescript_call_keeps_calls_edge_only() {
    let g = ts_graph("obj.m(1);");
    assert_call_only(&g, "run", "cls.ts", "m");
}

/// Stands for: a parenthesized callee `(obj.m)(1)` / `(this.m)()` is a call,
/// never a method value, and records the `Calls` edge `obj.m(1)` records.
#[test]
fn test_bound_method_reference_typescript_parenthesized_call_keeps_calls_edge_only() {
    let g = ts_graph("(obj.m)(1);");
    assert_call_only(&g, "run", "cls.ts", "m");
}

/// Stands for: a parenthesized `(this.m)()` is a call and records its `Calls`
/// edge, never a method value.
#[test]
fn test_bound_method_reference_typescript_parenthesized_this_call_keeps_calls_edge_only() {
    let src = "class C {\n  m(): void {}\n  run(): void {\n    (this.m)();\n  }\n}\n";
    let g = graph(&TypeScriptProvider::new().unwrap(), &[("c.ts", src)]);
    assert_call_only(&g, "run", "c.ts", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_typescript_same_reference_twice_returns_one_edge() {
    let g = ts_graph("xs.forEach(obj.m);\n  xs.forEach(obj.m);");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

/// An empty and a whitespace-only file in the tree change nothing.
#[test]
fn test_bound_method_reference_typescript_empty_files_keep_the_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("cls.ts", TS_CLS),
            ("empty.ts", ""),
            ("blank.ts", "  \n\t\n"),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "selfRef"), "cls.ts", "m");
}

// ---- JavaScript ----

const JS_CLS: &str = "export class Cls {\n  m(x) {}\n  selfRef(xs) {\n    xs.forEach(this.m);\n  }\n  selfBind(xs) {\n    xs.forEach(this.m.bind(this));\n  }\n  callIt() {\n    this.m(1);\n  }\n}\n";
const JS_OTHER: &str = "export class Other {\n  m(x) {}\n}\n";
const JS_USE: &str =
    "import { Cls } from './cls.js';\nexport function loose(xs, obj) {\n  xs.forEach(obj.m);\n}\n";

fn js_graph() -> ZeroCopyGraph {
    graph(
        &JavaScriptProvider::new().unwrap(),
        &[
            ("cls.js", JS_CLS),
            ("other.js", JS_OTHER),
            ("use.js", JS_USE),
        ],
    )
}

#[test]
fn test_bound_method_reference_javascript_this_receiver_binds_enclosing_class() {
    assert_one_cross_file(&function_refs(&js_graph(), "selfRef"), "cls.js", "m");
}

#[test]
fn test_bound_method_reference_javascript_bind_this_binds_enclosing_class() {
    assert_one_cross_file(&function_refs(&js_graph(), "selfBind"), "cls.js", "m");
}

#[test]
fn test_bound_method_reference_javascript_untyped_receiver_returns_no_edge() {
    assert_eq!(function_refs(&js_graph(), "loose"), none());
}

#[test]
fn test_bound_method_reference_javascript_call_keeps_calls_edge_only() {
    assert_call_only(&js_graph(), "callIt", "cls.js", "m");
}

/// Stands for: a parenthesized callee `(c.m)(1)` is a call, never a method
/// value, and records the `Calls` edge `c.m(1)` records.
#[test]
fn test_bound_method_reference_javascript_parenthesized_this_call_keeps_calls_edge_only() {
    let src = "export class C {\n  m() {}\n  run() {\n    (this.m)();\n  }\n}\n";
    let g = graph(&JavaScriptProvider::new().unwrap(), &[("c.js", src)]);
    assert_call_only(&g, "run", "c.js", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_javascript_same_reference_twice_returns_one_edge() {
    let src = "export class C {\n  m() {}\n  run(xs) {\n    xs.forEach(this.m);\n    xs.forEach(this.m);\n  }\n}\n";
    let found = refs(&JavaScriptProvider::new().unwrap(), &[("c.js", src)], "run");
    assert_eq!(found.len(), 1, "{found:?}");
}

// ---- Ruby ----

const RB_CLS: &str =
    "class Cls\n  def m(x); end\n\n  def self_ref(xs)\n    xs.each(&method(:m))\n  end\nend\n";
const RB_OTHER: &str = "class Other\n  def m(x); end\nend\n";
const RB_USE: &str = "def built(xs)\n  xs.each(&Cls.new.method(:m))\nend\n\ndef loose(xs, obj)\n  xs.each(&obj.method(:m))\nend\n";

fn ruby_graph() -> ZeroCopyGraph {
    graph(
        &RubyProvider::new().unwrap(),
        &[
            ("cls.rb", RB_CLS),
            ("other.rb", RB_OTHER),
            ("use.rb", RB_USE),
        ],
    )
}

#[test]
fn test_bound_method_reference_ruby_constructed_receiver_binds_its_type() {
    assert_one_cross_file(&function_refs(&ruby_graph(), "built"), "cls.rb", "m");
}

/// Stands for: handing one method on twice from one function is one edge.
#[test]
fn test_bound_method_reference_ruby_same_reference_twice_returns_one_edge() {
    let twice =
        "def twice(xs)\n  xs.each(&Cls.new.method(:m))\n  xs.each(&Cls.new.method(:m))\nend\n";
    let g = graph(
        &RubyProvider::new().unwrap(),
        &[("cls.rb", RB_CLS), ("use.rb", twice)],
    );
    assert_one_cross_file(&function_refs(&g, "twice"), "cls.rb", "m");
}

#[test]
fn test_bound_method_reference_ruby_implicit_self_binds_enclosing_class() {
    assert_one_cross_file(&function_refs(&ruby_graph(), "self_ref"), "cls.rb", "m");
}

#[test]
fn test_bound_method_reference_ruby_untyped_receiver_returns_no_edge() {
    assert_eq!(function_refs(&ruby_graph(), "loose"), none());
}

// ---- Rust ----

/// `s.f` without a call reads a field; Rust has no bound method value.
#[test]
fn test_bound_method_reference_rust_field_access_returns_no_edge() {
    let src = "struct S {\n    f: fn(i32),\n}\n\nimpl S {\n    fn m(&self, x: i32) {}\n}\n\nfn run(s: S) {\n    let g = s.f;\n    g(1);\n}\n";
    let found = refs(&RustProvider::new().unwrap(), &[("lib.rs", src)], "run");
    assert_eq!(found, none());
}

/// Stands for: `pair.0` indexes a plain tuple; it is no read of the field "0"
/// of an unrelated tuple struct in the repo (main: no row).
#[test]
fn test_bound_method_reference_rust_tuple_index_read_returns_no_value_read() {
    let src = "struct Wrapper(u8, u8);\n\nfn first(pair: (u8, u8)) -> u8 {\n    pair.0\n}\n";
    let g = graph(&RustProvider::new().unwrap(), &[("lib.rs", src)]);
    let found = name_ref_support::refs(&g, "first", RelType::ReadsField, "field_read");
    assert_eq!(found, none());
}

// ---- Receiver typing respects scope, `this` and labels ----

/// The owner class of each method `source` reaches by `rel` with `reason`,
/// sorted: it tells `Outer.cb` from `Inner.cb`, which share a name.
fn target_owners(g: &ZeroCopyGraph, source: &str, rel: RelType, reason: &str) -> Vec<String> {
    let pool = g.string_pool.as_slice();
    let mut owners: Vec<String> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel && e.reason.resolve(pool) == reason)
        .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == source)
        .map(|e| {
            g.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_string()
        })
        .collect();
    owners.sort();
    owners
}

fn reference_owners(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
    target_owners(g, source, RelType::References, "function_reference")
}

// Stands for: a name that a nearer function declares is not the outer
// annotated one. Each case below reads `obj.m` in an inner function of
// `run(xs, obj: Cls, loose)`; the untyped ones bind nothing.
#[test]
fn test_bound_method_reference_python_untyped_inner_parameter_returns_no_edge() {
    let g = python_graph("def inner(obj):\n        return list(map(obj.m, xs))\n    return inner");
    assert_eq!(function_refs(&g, "inner"), none());
}

#[test]
fn test_bound_method_reference_python_inner_assignment_shadows_outer_type_returns_no_edge() {
    let g = python_graph(
        "def inner():\n        obj = loose\n        return list(map(obj.m, xs))\n    return inner",
    );
    assert_eq!(function_refs(&g, "inner"), none());
}

#[test]
fn test_bound_method_reference_python_inner_function_without_own_binding_keeps_outer_type() {
    let g = python_graph("def inner():\n        return list(map(obj.m, xs))\n    return inner");
    assert_one_cross_file(&function_refs(&g, "inner"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_inner_annotation_wins_over_outer_type() {
    let g = python_graph(
        "def inner(obj: Other):\n        return list(map(obj.m, xs))\n    return inner",
    );
    assert_one_cross_file(&function_refs(&g, "inner"), "other.py", "m");
}

#[test]
fn test_bound_method_reference_typescript_untyped_inner_parameter_returns_no_edge() {
    let g = ts_graph(
        "const f = (obj: any) => xs.forEach(obj.m);\n  const g = (obj) => xs.forEach(obj.m);",
    );
    assert_eq!(function_refs(&g, "f"), none());
    assert_eq!(function_refs(&g, "g"), none());
}

#[test]
fn test_bound_method_reference_typescript_destructured_inner_parameter_returns_no_edge() {
    let g = ts_graph("const d = ({ obj }: any) => xs.forEach(obj.m);");
    assert_eq!(function_refs(&g, "d"), none());
}

#[test]
fn test_bound_method_reference_typescript_inner_local_declaration_returns_no_edge() {
    let g = ts_graph("const h = () => {\n    const obj = loose;\n    xs.forEach(obj.m);\n  };");
    assert_eq!(function_refs(&g, "h"), none());
}

#[test]
fn test_bound_method_reference_typescript_inner_function_without_own_binding_keeps_outer_type() {
    let g = ts_graph("const k = () => xs.forEach(obj.m);");
    assert_one_cross_file(&function_refs(&g, "k"), "cls.ts", "m");
}

/// Stands for: a lambda parameter, a comprehension variable, a `for` target
/// and a `with ... as` name each declare `obj` anew, so the outer `obj: Cls`
/// does not type the receiver.
#[test]
fn test_bound_method_reference_python_lambda_parameter_shadows_outer_type_returns_no_edge() {
    let g = python_graph("f = lambda obj: list(map(obj.m, xs))");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_python_comprehension_variable_shadows_outer_type_returns_no_edge() {
    let g = python_graph("return [list(map(obj.m, xs)) for obj in loose]");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_python_for_target_shadows_outer_type_returns_no_edge() {
    let g = python_graph(
        "def inner():\n        for obj in loose:\n            list(map(obj.m, xs))\n    return inner",
    );
    assert_eq!(function_refs(&g, "inner"), none());
}

#[test]
fn test_bound_method_reference_python_with_as_name_shadows_outer_type_returns_no_edge() {
    let g = python_graph(
        "def inner():\n        with loose as obj:\n            list(map(obj.m, xs))\n    return inner",
    );
    assert_eq!(function_refs(&g, "inner"), none());
}

/// Stands for: a statement in the function that declares `obj: Cls` gives
/// `obj` a new value from its own body on: a `for` target, an `except` or
/// `with` name, a `case` capture, a walrus and a plain assignment.
#[test]
fn test_bound_method_reference_python_rebinding_in_declaring_function_returns_no_edge() {
    for body in [
        "for obj in loose:\n        list(map(obj.m, xs))",
        "try:\n        pass\n    except ValueError as obj:\n        list(map(obj.m, xs))",
        "with loose as obj:\n        list(map(obj.m, xs))",
        "match loose:\n        case [obj]:\n            list(map(obj.m, xs))",
        "if (obj := loose):\n        list(map(obj.m, xs))",
        "obj = loose\n    return list(map(obj.m, xs))",
        "for obj in loose:\n        pass\n    return list(map(obj.m, xs))",
    ] {
        let g = python_graph(body);
        assert_eq!(function_refs(&g, "run"), none(), "{body}");
    }
}

/// Stands for: the expression a rebinding statement evaluates, and every read
/// before the statement, still see `obj: Cls`.
#[test]
fn test_bound_method_reference_python_read_before_rebinding_keeps_type() {
    for body in [
        "for obj in map(obj.m, xs):\n        pass",
        "with map(obj.m, xs) as obj:\n        pass",
        "obj = map(obj.m, xs)",
        "if (obj := map(obj.m, xs)):\n        pass",
        "f = map(obj.m, xs)\n    obj = loose",
    ] {
        let g = python_graph(body);
        assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
    }
}

/// Stands for: a lambda or comprehension that binds another name leaves the
/// outer `obj: Cls` typing the receiver.
#[test]
fn test_bound_method_reference_python_lambda_and_comprehension_of_other_name_keep_outer_type() {
    let g = python_graph("f = lambda y: list(map(obj.m, xs))");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
    let g = python_graph("return [list(map(obj.m, xs)) for y in loose]");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

/// Stands for: a lambda default and the first iterable of a comprehension run
/// in the enclosing scope, so the outer `obj: Cls` types `obj.m` there even
/// when the lambda or comprehension binds `obj`.
#[test]
fn test_bound_method_reference_python_lambda_default_reads_outer_type() {
    let g = python_graph("return lambda obj=obj.m: obj(1)");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_comprehension_first_iterable_reads_outer_type() {
    let g = python_graph("return [obj for obj in map(obj.m, xs)]");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.py", "m");
}

/// Stands for: a comprehension variable is visible in the conditions and in
/// every later `for` clause.
#[test]
fn test_bound_method_reference_python_comprehension_condition_returns_no_edge() {
    let g = python_graph("return [1 for obj in loose if map(obj.m, xs)]");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_python_comprehension_later_iterable_returns_no_edge() {
    let g = python_graph("return [1 for obj in loose for y in map(obj.m, xs)]");
    assert_eq!(function_refs(&g, "run"), none());
}

/// Stands for: a `nonlocal` name is the outer variable, so the inner function
/// makes no new local and the outer `obj: Cls` types `obj.m`. A write to it
/// replaces that value from the write on.
#[test]
fn test_bound_method_reference_python_nonlocal_name_keeps_outer_type() {
    let g = python_graph(
        "def inner():\n        nonlocal obj\n        return list(map(obj.m, xs))\n    return inner",
    );
    assert_one_cross_file(&function_refs(&g, "inner"), "cls.py", "m");
}

#[test]
fn test_bound_method_reference_python_nonlocal_name_written_returns_no_edge() {
    let g = python_graph(
        "def inner():\n        nonlocal obj\n        for obj in loose:\n            pass\n        return list(map(obj.m, xs))\n    return inner",
    );
    assert_eq!(function_refs(&g, "inner"), none());
}

/// Stands for: a `global` name is a module binding, never the enclosing
/// function's `obj: Cls`.
#[test]
fn test_bound_method_reference_python_global_name_returns_no_edge() {
    let g = python_graph(
        "def inner():\n        global obj\n        return list(map(obj.m, xs))\n    return inner",
    );
    assert_eq!(function_refs(&g, "inner"), none());
}

/// Stands for: a `case` capture binds a new local: a bare name, a keyword
/// pattern's value, a star pattern and an `as` name.
#[test]
fn test_bound_method_reference_python_case_captures_return_no_edge() {
    for pattern in [
        "[obj]",
        "Cls(x=obj)",
        "[_, *obj]",
        "Cls() as obj",
        "{'k': obj}",
    ] {
        let g = python_graph(&format!(
            "def inner():\n        match loose:\n            case {pattern}:\n                return list(map(obj.m, xs))\n    return inner"
        ));
        assert_eq!(function_refs(&g, "inner"), none(), "case {pattern}");
    }
}

/// Stands for: a dotted value pattern and a keyword pattern's keyword name
/// bind nothing, so the outer `obj: Cls` types `obj.m`.
#[test]
fn test_bound_method_reference_python_case_value_patterns_keep_outer_type() {
    for pattern in ["Cls.m", "Cls(obj=1)"] {
        let g = python_graph(&format!(
            "def inner():\n        match loose:\n            case {pattern}:\n                return list(map(obj.m, xs))\n    return inner"
        ));
        assert_one_cross_file(&function_refs(&g, "inner"), "cls.py", "m");
    }
}

/// Stands for: a destructuring default `{ y = obj }` and a computed key
/// `{ [obj]: v }` read `obj`, they do not declare it, so the outer
/// `obj: Cls` still types `obj.m`.
#[test]
fn test_bound_method_reference_typescript_destructuring_default_keeps_outer_type() {
    let g = ts_graph("function inner({ y = obj }: any) {\n    return xs.forEach(obj.m);\n  }");
    assert_one_cross_file(&function_refs(&g, "inner"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_destructuring_computed_key_keeps_outer_type() {
    let g = ts_graph("const c = ({ [obj]: v }: any) => xs.forEach(obj.m);");
    assert_one_cross_file(&function_refs(&g, "c"), "cls.ts", "m");
}

/// Stands for: a destructuring default's own left name is a binding, so
/// `({ obj = 1 })` rebinds `obj`.
#[test]
fn test_bound_method_reference_typescript_destructuring_default_left_name_returns_no_edge() {
    let g = ts_graph("const d = ({ obj = 1 }: any) => xs.forEach(obj.m);");
    assert_eq!(function_refs(&g, "d"), none());
}

/// Stands for: `for (const obj of ...)` and `catch (obj)` declare `obj` anew.
#[test]
fn test_bound_method_reference_typescript_for_of_variable_shadows_outer_type_returns_no_edge() {
    let g = ts_graph("for (const obj of loose) {\n    xs.forEach(obj.m);\n  }");
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_typescript_catch_parameter_shadows_outer_type_returns_no_edge() {
    let g = ts_graph("try {\n    xs.length;\n  } catch (obj) {\n    xs.forEach(obj.m);\n  }");
    assert_eq!(function_refs(&g, "run"), none());
}

/// Stands for: a `for ... of` over another name leaves the outer typing.
#[test]
fn test_bound_method_reference_typescript_for_of_other_variable_keeps_outer_type() {
    let g = ts_graph("for (const z of loose) {\n    xs.forEach(obj.m);\n  }");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

/// Stands for: `for (obj of ...)` and `for (obj in ...)` assign each element to
/// `obj`, so inside the loop the outer `obj: Cls` does not type `obj.m`. A
/// member target `for (h.obj of ...)` writes no name.
#[test]
fn test_bound_method_reference_typescript_for_of_assignment_target_returns_no_edge() {
    for head in ["for (obj of loose)", "for (obj in loose)"] {
        let g = ts_graph(&format!("{head} {{\n    xs.forEach(obj.m);\n  }}"));
        assert_eq!(function_refs(&g, "run"), none(), "{head}");
    }
}

/// Stands for: a destructuring or parenthesized target also assigns `obj`, and
/// the new value stays after the loop ends.
#[test]
fn test_bound_method_reference_typescript_for_of_pattern_target_returns_no_edge() {
    for body in [
        "for ([obj] of loose) {\n    xs.forEach(obj.m);\n  }",
        "for ({ obj } of loose) {\n    xs.forEach(obj.m);\n  }",
        "for ((obj) of loose) {\n    xs.forEach(obj.m);\n  }",
        "for (obj of loose) {}\n  xs.forEach(obj.m);",
    ] {
        let g = ts_graph(body);
        assert_eq!(function_refs(&g, "run"), none(), "{body}");
    }
}

/// Stands for: the iterable of `for (obj of ...)` runs before the first
/// assignment to `obj`, so it still reads `obj: Cls`.
#[test]
fn test_bound_method_reference_typescript_for_of_iterable_reads_outer_type() {
    let g = ts_graph("for (obj of xs.map(obj.m)) {}");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

#[test]
fn test_bound_method_reference_typescript_for_of_member_target_keeps_outer_type() {
    let g = ts_graph("for (loose.obj of xs) {\n    xs.forEach(obj.m);\n  }");
    assert_one_cross_file(&function_refs(&g, "run"), "cls.ts", "m");
}

// Stands for: `this` is the class only in a method of its body (or a field
// initializer, or an arrow chain up to one); a plain `function` and an
// object-literal method bind their own `this`, so neither names `C.m`.
const THIS_NESTED_TS: &str = "class C {\n  m(): void {}\n  go(): void {\n    function pick() { return this.m; }\n    const o = { run() { return this.m; } };\n    const ok = () => this.m;\n  }\n}\n";

#[test]
fn test_bound_method_reference_typescript_this_in_nested_function_returns_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[("c.ts", THIS_NESTED_TS)],
    );
    assert_eq!(function_refs(&g, "pick"), none());
}

#[test]
fn test_bound_method_reference_typescript_this_in_object_literal_method_returns_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[("c.ts", THIS_NESTED_TS)],
    );
    assert_eq!(function_refs(&g, "run"), none());
}

#[test]
fn test_bound_method_reference_typescript_this_in_arrow_inside_method_binds_enclosing_class() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[("c.ts", THIS_NESTED_TS)],
    );
    assert_one_cross_file(&function_refs(&g, "ok"), "c.ts", "m");
}

#[test]
fn test_bound_method_reference_javascript_this_in_nested_function_returns_no_edge() {
    let src = "export class C {\n  m() {}\n  go() {\n    function pick() { return this.m; }\n    const o = { run() { return this.m; } };\n    const ok = () => this.m;\n  }\n}\n";
    let g = graph(&JavaScriptProvider::new().unwrap(), &[("c.js", src)]);
    assert_eq!(function_refs(&g, "pick"), none());
    assert_eq!(function_refs(&g, "run"), none());
    assert_one_cross_file(&function_refs(&g, "ok"), "c.js", "m");
}

/// A file with no `this` read builds nothing and binds nothing (the lazy
/// member tables).
#[test]
fn test_bound_method_reference_typescript_file_without_this_reads_returns_no_edge() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[(
            "f.ts",
            "export function plain(xs: number[]): void { xs.forEach(String); }\n",
        )],
    );
    assert_eq!(function_refs(&g, "plain"), none());
}

// Stands for: an explicit type argument on a method reference or method group
// does not change which method it names.
#[test]
fn test_bound_method_reference_java_explicit_type_argument_binds_its_type() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("Cls.java", JAVA_CLS),
            ("Use.java", java_use("xs.forEach(c::<Integer>m);").as_str()),
        ],
    );
    assert_one_cross_file(&function_refs(&g, "run"), "Cls.java", "m");
}

#[test]
fn test_bound_method_reference_csharp_generic_method_group_binds_its_type() {
    let g = csharp_graph("xs.ForEach(c.M<int>);");
    assert_one_cross_file(&function_refs(&g, "Run"), "Cls.cs", "M");
}

// Stands for: `this@Outer` names the enclosing class labelled `Outer`, not the
// innermost class; a label no enclosing class carries names nothing. The call
// `this@Outer.cb()` shares the helper and follows the same rule.
const KT_LABELLED: &str = "class Outer {\n    fun cb() {}\n    inner class Inner {\n        fun cb() {}\n        fun refOuter() { val a = this@Outer::cb }\n        fun refSelf() { val b = this::cb }\n        fun refNope() { val c = this@Nope::cb }\n        fun callOuter() { this@Outer.cb() }\n        fun callSelf() { this.cb() }\n    }\n}\n";

fn kotlin_labelled() -> ZeroCopyGraph {
    graph(&KotlinProvider::new().unwrap(), &[("o.kt", KT_LABELLED)])
}

#[test]
fn test_bound_method_reference_kotlin_labelled_this_binds_the_labelled_class() {
    assert_eq!(reference_owners(&kotlin_labelled(), "refOuter"), ["Outer"]);
}

#[test]
fn test_bound_method_reference_kotlin_plain_this_binds_the_innermost_class() {
    assert_eq!(reference_owners(&kotlin_labelled(), "refSelf"), ["Inner"]);
}

#[test]
fn test_bound_method_reference_kotlin_unknown_label_returns_no_edge() {
    assert_eq!(
        reference_owners(&kotlin_labelled(), "refNope"),
        Vec::<String>::new()
    );
}

#[test]
fn test_bound_method_reference_kotlin_labelled_this_call_binds_the_labelled_class() {
    let g = kotlin_labelled();
    assert_eq!(
        target_owners(&g, "callOuter", RelType::Calls, "call"),
        ["Outer"]
    );
    assert_eq!(
        target_owners(&g, "callSelf", RelType::Calls, "call"),
        ["Inner"]
    );
}
