//! A call that sits in no Function, Method or Constructor takes a holder:
//! the smallest enclosing value declaration (Property, Variable, Const),
//! else the smallest enclosing type. A statement that declares two or more
//! values gives the call no holder. A call inside a callable keeps that
//! callable as its holder.

use crate::name_ref_support::{calls, graph};
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
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};

fn holds(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    holder: &str,
    target: &str,
) -> bool {
    let g = graph(provider, &[(path, src)]);
    calls(&g, holder).iter().any(|(_, name)| name == target)
}

fn assert_holds(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    holder: &str,
    target: &str,
) {
    let g = graph(provider, &[(path, src)]);
    let got = calls(&g, holder);
    assert!(
        got.iter().any(|(_, name)| name == target),
        "{holder} must hold a call of {target}, got {got:?}"
    );
}

fn assert_not_holds(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    holder: &str,
    target: &str,
) {
    assert!(
        !holds(provider, path, src, holder, target),
        "{holder} must not hold a call of {target}"
    );
}

/// `(source kind, source name)` of every Calls edge into `target`.
fn callers_of(g: &ZeroCopyGraph, target: &str) -> Vec<(NodeKind, String)> {
    let pool = g.string_pool.as_slice();
    g.edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| g.nodes[e.target as usize].name.resolve(pool) == target)
        .map(|e| {
            let s = &g.nodes[e.source as usize];
            (s.kind, s.name.resolve(pool).to_string())
        })
        .collect()
}

#[test]
fn test_java_static_block_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} static { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_java_instance_block_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_java_field_initializer_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static int target() { return 1; } static int F = target(); }",
        "F",
        "target",
    );
}

#[test]
fn test_csharp_property_getter_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } int X { get { return Target(); } } }",
        "X",
        "Target",
    );
}

#[test]
fn test_csharp_property_setter_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static void Target() {} int X { set { Target(); } } }",
        "X",
        "Target",
    );
}

#[test]
fn test_csharp_expression_bodied_property_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } int X => Target(); }",
        "X",
        "Target",
    );
}

#[test]
fn test_csharp_field_initializer_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } static readonly int F = Target(); }",
        "F",
        "Target",
    );
}

#[test]
fn test_kotlin_init_block_call_gets_holder() {
    assert_holds(
        &KotlinProvider::new().unwrap(),
        "T.kt",
        "fun target() {}\nclass T { init { target() } }",
        "T",
        "target",
    );
}

#[test]
fn test_kotlin_property_getter_call_gets_holder() {
    assert_holds(
        &KotlinProvider::new().unwrap(),
        "T.kt",
        "fun target(): Int = 1\nclass T { val x: Int get() = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_swift_computed_property_call_gets_holder() {
    assert_holds(
        &SwiftProvider::new().unwrap(),
        "T.swift",
        "func target() -> Int { 1 }\nclass T { var x: Int { target() } }",
        "x",
        "target",
    );
}

#[test]
fn test_swift_did_set_call_gets_holder() {
    assert_holds(
        &SwiftProvider::new().unwrap(),
        "T.swift",
        "func target() {}\nclass T { var x: Int = 0 { didSet { target() } } }",
        "x",
        "target",
    );
}

#[test]
fn test_typescript_static_block_call_gets_holder() {
    assert_holds(
        &TypeScriptProvider::new().unwrap(),
        "T.ts",
        "function target() {}\nclass T { static { target() } }",
        "T",
        "target",
    );
}

#[test]
fn test_typescript_field_initializer_call_gets_holder() {
    assert_holds(
        &TypeScriptProvider::new().unwrap(),
        "T.ts",
        "function target() { return 1 }\nclass T { x = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_javascript_field_initializer_call_gets_holder() {
    assert_holds(
        &JavaScriptProvider::new().unwrap(),
        "T.js",
        "function target() { return 1 }\nclass T { x = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_python_class_attribute_call_gets_holder() {
    assert_holds(
        &PythonProvider::new().unwrap(),
        "t.py",
        "def target(): return 1\nclass T:\n    x = target()\n",
        "x",
        "target",
    );
}

#[test]
fn test_ruby_class_body_call_gets_holder() {
    assert_holds(
        &RubyProvider::new().unwrap(),
        "t.rb",
        "def target(name); end\nclass T\n  target :x\nend\n",
        "T",
        "target",
    );
}

#[test]
fn test_dart_getter_call_gets_holder() {
    assert_holds(
        &DartProvider::new().unwrap(),
        "t.dart",
        "int target() => 1;\nclass T { int get x => target(); }",
        "x",
        "target",
    );
}

#[test]
fn test_go_package_var_call_gets_holder() {
    assert_holds(
        &GoProvider::new().unwrap(),
        "t.go",
        "package p\nfunc target() int { return 1 }\nvar X = target()\n",
        "X",
        "target",
    );
}

#[test]
fn test_rust_const_call_gets_holder() {
    assert_holds(
        &RustProvider::new().unwrap(),
        "src/lib.rs",
        "const fn target() -> i32 { 1 }\nconst X: i32 = target();",
        "X",
        "target",
    );
}

#[test]
fn test_cpp_member_initializer_call_gets_holder() {
    assert_holds(
        &CppProvider::new().unwrap(),
        "t.cpp",
        "int target() { return 1; }\nstruct T { int x = target(); };",
        "x",
        "target",
    );
}

#[test]
fn test_c_global_initializer_call_gets_holder() {
    assert_holds(
        &CProvider::new().unwrap(),
        "t.c",
        "int target(void) { return 1; }\nint g = target();",
        "g",
        "target",
    );
}

#[test]
fn test_java_method_call_keeps_method_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} void m() { target(); } }",
        "m",
        "target",
    );
}

#[test]
fn test_java_method_call_enclosing_class_not_holder() {
    assert_not_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} void m() { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_csharp_method_call_keeps_method_holder() {
    let p = CSharpProvider::new().unwrap();
    let src = "class T { static void Target() {} void M() { Target(); } }";
    assert_holds(&p, "T.cs", src, "M", "Target");
    assert_not_holds(&p, "T.cs", src, "T", "Target");
}

#[test]
fn test_php_method_call_keeps_method_holder() {
    let p = PhpProvider::new().unwrap();
    let src = "<?php\nfunction target() {}\nclass T { public function m() { target(); } }";
    assert_holds(&p, "t.php", src, "m", "target");
    assert_not_holds(&p, "t.php", src, "T", "target");
}

#[test]
fn test_python_method_call_keeps_method_holder() {
    let p = PythonProvider::new().unwrap();
    let src = "def target(): return 1\nclass T:\n    def m(self):\n        target()\n";
    assert_holds(&p, "t.py", src, "m", "target");
    assert_not_holds(&p, "t.py", src, "T", "target");
}

#[test]
fn test_go_function_call_keeps_function_holder() {
    let p = GoProvider::new().unwrap();
    let src =
        "package p\nfunc target() int { return 1 }\nfunc f() int { return target() }\nvar X = 0\n";
    assert_holds(&p, "t.go", src, "f", "target");
    assert_not_holds(&p, "t.go", src, "X", "target");
}

#[test]
/// An arrow function is a Function node, so it outranks the `const` that
/// names it. (A TS class field `h = () => g()` has no Function node, so its
/// Property holds the call.)
fn test_typescript_arrow_const_call_stays_on_arrow_not_const() {
    let p = TypeScriptProvider::new().unwrap();
    let g = graph(
        &p,
        &[("T.ts", "function target() {}\nconst f = () => target();")],
    );
    let callers = callers_of(&g, "target");
    assert_eq!(callers, vec![(NodeKind::Function, "f".to_string())]);
}

#[test]
fn test_go_shared_var_statement_call_gets_no_holder() {
    let p = GoProvider::new().unwrap();
    let src = "package p\nfunc h1() int { return 1 }\nfunc h2() int { return 2 }\nvar C, D = h1(), h2()\n";
    for holder in ["C", "D"] {
        for target in ["h1", "h2"] {
            assert_not_holds(&p, "t.go", src, holder, target);
        }
    }
}

#[test]
fn test_javascript_shared_const_statement_call_gets_no_holder() {
    let p = JavaScriptProvider::new().unwrap();
    let src = "function h1() {}\nfunction h2() {}\nconst a = h1(), b = h2();";
    for holder in ["a", "b"] {
        for target in ["h1", "h2"] {
            assert_not_holds(&p, "t.js", src, holder, target);
        }
    }
}

#[test]
fn test_java_empty_static_block_holds_no_edge() {
    let p = JavaProvider::new().unwrap();
    let g = graph(&p, &[("T.java", "class T { static {} }")]);
    assert!(calls(&g, "T").is_empty());
}

#[test]
fn test_java_static_block_undefined_call_holds_no_edge() {
    let p = JavaProvider::new().unwrap();
    let g = graph(&p, &[("T.java", "class T { static { missing(); } }")]);
    assert!(calls(&g, "T").is_empty());
}

#[test]
fn test_java_static_block_two_calls_one_holder() {
    let p = JavaProvider::new().unwrap();
    let src = "class T { static void a() {} static void b() {} static { a(); b(); } }";
    assert_holds(&p, "T.java", src, "T", "a");
    assert_holds(&p, "T.java", src, "T", "b");
}

#[test]
fn test_java_nested_class_static_block_holds_by_nested_class() {
    let p = JavaProvider::new().unwrap();
    let src = "class O { static void target() {} static class N { static { target(); } } }";
    assert_holds(&p, "O.java", src, "N", "target");
    assert_not_holds(&p, "O.java", src, "O", "target");
}
