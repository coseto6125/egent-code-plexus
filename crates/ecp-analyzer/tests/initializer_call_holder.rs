//! A call that sits in no Function, Method or Constructor takes a holder:
//! the smallest enclosing value declaration (Property, Variable, Const),
//! else the smallest enclosing type. A statement that declares two or more
//! values gives the call to the declarator's own value, or to the type when
//! the declarator names no value node. A call inside a callable keeps that
//! callable as its holder.
//!
//! PHP property defaults and class constants take constant expressions, so
//! they hold no function call. PHP 8.1 allows `new` in a global `const`
//! initializer: the `const_declaration` query emits a Const node per
//! `const_element`, and that Const holds the constructor call.
//!
//! No multi-value statement with call initializers exists in Python (one
//! target per assignment statement), Ruby (no multi-declarator form), PHP
//! (constant expressions only), Rust (one binding per `const`/`static`) and
//! Kotlin (one property per declaration). Top-level script code (Python, JS,
//! TS, PHP, Ruby, Swift statements outside every declaration) is a separate
//! open gap.

use crate::name_ref_support::{calls, function_refs, graph, value_reads};
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
fn test_build_java_static_block_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} static { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_build_java_instance_block_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_build_java_field_initializer_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static int target() { return 1; } static int F = target(); }",
        "F",
        "target",
    );
}

#[test]
fn test_build_csharp_property_getter_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } int X { get { return Target(); } } }",
        "X",
        "Target",
    );
}

#[test]
fn test_build_csharp_property_setter_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static void Target() {} int X { set { Target(); } } }",
        "X",
        "Target",
    );
}

#[test]
fn test_build_csharp_expression_bodied_property_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } int X => Target(); }",
        "X",
        "Target",
    );
}

#[test]
fn test_build_csharp_field_initializer_call_gets_holder() {
    assert_holds(
        &CSharpProvider::new().unwrap(),
        "T.cs",
        "class T { static int Target() { return 1; } static readonly int F = Target(); }",
        "F",
        "Target",
    );
}

#[test]
fn test_build_kotlin_init_block_call_gets_holder() {
    assert_holds(
        &KotlinProvider::new().unwrap(),
        "T.kt",
        "fun target() {}\nclass T { init { target() } }",
        "T",
        "target",
    );
}

#[test]
fn test_build_kotlin_property_getter_call_gets_holder() {
    assert_holds(
        &KotlinProvider::new().unwrap(),
        "T.kt",
        "fun target(): Int = 1\nclass T { val x: Int get() = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_build_swift_computed_property_call_gets_holder() {
    assert_holds(
        &SwiftProvider::new().unwrap(),
        "T.swift",
        "func target() -> Int { 1 }\nclass T { var x: Int { target() } }",
        "x",
        "target",
    );
}

#[test]
fn test_build_swift_did_set_call_gets_holder() {
    assert_holds(
        &SwiftProvider::new().unwrap(),
        "T.swift",
        "func target() {}\nclass T { var x: Int = 0 { didSet { target() } } }",
        "x",
        "target",
    );
}

#[test]
fn test_build_typescript_static_block_call_gets_holder() {
    assert_holds(
        &TypeScriptProvider::new().unwrap(),
        "T.ts",
        "function target() {}\nclass T { static { target() } }",
        "T",
        "target",
    );
}

#[test]
fn test_build_typescript_field_initializer_call_gets_holder() {
    assert_holds(
        &TypeScriptProvider::new().unwrap(),
        "T.ts",
        "function target() { return 1 }\nclass T { x = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_build_javascript_field_initializer_call_gets_holder() {
    assert_holds(
        &JavaScriptProvider::new().unwrap(),
        "T.js",
        "function target() { return 1 }\nclass T { x = target() }",
        "x",
        "target",
    );
}

#[test]
fn test_build_python_class_attribute_call_gets_holder() {
    assert_holds(
        &PythonProvider::new().unwrap(),
        "t.py",
        "def target(): return 1\nclass T:\n    x = target()\n",
        "x",
        "target",
    );
}

#[test]
fn test_build_ruby_class_body_call_gets_holder() {
    assert_holds(
        &RubyProvider::new().unwrap(),
        "t.rb",
        "def target(name); end\nclass T\n  target :x\nend\n",
        "T",
        "target",
    );
}

#[test]
fn test_build_dart_getter_call_gets_holder() {
    assert_holds(
        &DartProvider::new().unwrap(),
        "t.dart",
        "int target() => 1;\nclass T { int get x => target(); }",
        "x",
        "target",
    );
}

#[test]
fn test_build_go_package_var_call_gets_holder() {
    assert_holds(
        &GoProvider::new().unwrap(),
        "t.go",
        "package p\nfunc target() int { return 1 }\nvar X = target()\n",
        "X",
        "target",
    );
}

#[test]
fn test_build_rust_const_call_gets_holder() {
    assert_holds(
        &RustProvider::new().unwrap(),
        "src/lib.rs",
        "const fn target() -> i32 { 1 }\nconst X: i32 = target();",
        "X",
        "target",
    );
}

#[test]
fn test_build_cpp_member_initializer_call_gets_holder() {
    assert_holds(
        &CppProvider::new().unwrap(),
        "t.cpp",
        "int target() { return 1; }\nstruct T { int x = target(); };",
        "x",
        "target",
    );
}

#[test]
fn test_build_c_global_initializer_call_gets_holder() {
    assert_holds(
        &CProvider::new().unwrap(),
        "t.c",
        "int target(void) { return 1; }\nint g = target();",
        "g",
        "target",
    );
}

#[test]
fn test_build_java_method_call_keeps_method_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} void m() { target(); } }",
        "m",
        "target",
    );
}

#[test]
fn test_build_java_method_call_enclosing_class_not_holder() {
    assert_not_holds(
        &JavaProvider::new().unwrap(),
        "T.java",
        "class T { static void target() {} void m() { target(); } }",
        "T",
        "target",
    );
}

#[test]
fn test_build_csharp_method_call_keeps_method_holder() {
    let p = CSharpProvider::new().unwrap();
    let src = "class T { static void Target() {} void M() { Target(); } }";
    assert_holds(&p, "T.cs", src, "M", "Target");
    assert_not_holds(&p, "T.cs", src, "T", "Target");
}

#[test]
fn test_build_php_method_call_keeps_method_holder() {
    let p = PhpProvider::new().unwrap();
    let src = "<?php\nfunction target() {}\nclass T { public function m() { target(); } }";
    assert_holds(&p, "t.php", src, "m", "target");
    assert_not_holds(&p, "t.php", src, "T", "target");
}

#[test]
fn test_build_python_method_call_keeps_method_holder() {
    let p = PythonProvider::new().unwrap();
    let src = "def target(): return 1\nclass T:\n    def m(self):\n        target()\n";
    assert_holds(&p, "t.py", src, "m", "target");
    assert_not_holds(&p, "t.py", src, "T", "target");
}

#[test]
fn test_build_go_function_call_keeps_function_holder() {
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
fn test_build_typescript_arrow_const_call_stays_on_arrow_not_const() {
    let p = TypeScriptProvider::new().unwrap();
    let g = graph(
        &p,
        &[("T.ts", "function target() {}\nconst f = () => target();")],
    );
    let callers = callers_of(&g, "target");
    assert_eq!(callers, vec![(NodeKind::Function, "f".to_string())]);
}

#[test]
fn test_build_go_shared_var_statement_call_gets_own_declarator() {
    let p = GoProvider::new().unwrap();
    let src = "package p\nfunc h1() int { return 1 }\nfunc h2() int { return 2 }\nvar C, D = h1(), h2()\n";
    assert_holds(&p, "t.go", src, "C", "h1");
    assert_holds(&p, "t.go", src, "D", "h2");
    assert_not_holds(&p, "t.go", src, "C", "h2");
    assert_not_holds(&p, "t.go", src, "D", "h1");
}

#[test]
fn test_build_javascript_shared_const_statement_call_gets_own_declarator() {
    let p = JavaScriptProvider::new().unwrap();
    let src = "function h1() {}\nfunction h2() {}\nconst a = h1(), b = h2();";
    assert_holds(&p, "t.js", src, "a", "h1");
    assert_holds(&p, "t.js", src, "b", "h2");
    assert_not_holds(&p, "t.js", src, "a", "h2");
    assert_not_holds(&p, "t.js", src, "b", "h1");
}

#[test]
fn test_build_java_empty_static_block_holds_no_edge() {
    let p = JavaProvider::new().unwrap();
    let g = graph(&p, &[("T.java", "class T { static {} }")]);
    assert!(calls(&g, "T").is_empty());
}

#[test]
fn test_build_java_static_block_undefined_call_holds_no_edge() {
    let p = JavaProvider::new().unwrap();
    let g = graph(&p, &[("T.java", "class T { static { missing(); } }")]);
    assert!(calls(&g, "T").is_empty());
}

#[test]
fn test_build_java_static_block_two_calls_one_holder() {
    let p = JavaProvider::new().unwrap();
    let src = "class T { static void a() {} static void b() {} static { a(); b(); } }";
    assert_holds(&p, "T.java", src, "T", "a");
    assert_holds(&p, "T.java", src, "T", "b");
}

#[test]
fn test_build_java_nested_class_static_block_holds_by_nested_class() {
    let p = JavaProvider::new().unwrap();
    let src = "class O { static void target() {} static class N { static { target(); } } }";
    assert_holds(&p, "O.java", src, "N", "target");
    assert_not_holds(&p, "O.java", src, "O", "target");
}

/// Names of the owner class of every `Calls` target out of `caller`.
fn callee_owners(g: &ZeroCopyGraph, caller: &str) -> Vec<String> {
    let pool = g.string_pool.as_slice();
    g.edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == caller)
        .map(|e| {
            g.nodes[e.target as usize]
                .owner_class
                .resolve(pool)
                .to_owned()
        })
        .collect()
}

/// Names of every `EnumVariant` that a `References` edge reaches from `holder`.
fn variants_referenced(g: &ZeroCopyGraph, holder: &str) -> Vec<String> {
    let pool = g.string_pool.as_slice();
    g.edges
        .iter()
        .filter(|e| e.rel_type == RelType::References)
        .filter(|e| g.nodes[e.source as usize].name.resolve(pool) == holder)
        .filter(|e| g.nodes[e.target as usize].kind == NodeKind::EnumVariant)
        .map(|e| g.nodes[e.target as usize].name.resolve(pool).to_owned())
        .collect()
}

#[test]
fn test_build_javascript_pattern_declarator_call_gets_no_value_holder() {
    let p = JavaScriptProvider::new().unwrap();
    let g = graph(
        &p,
        &[(
            "t.js",
            "function target() { return {}; }\nconst unused = 0, { cb } = target();",
        )],
    );
    assert_eq!(callers_of(&g, "target"), vec![]);
}

#[test]
fn test_build_go_blank_name_shared_var_call_pairs_by_position() {
    let p = GoProvider::new().unwrap();
    let src = "package p\nfunc h1() int { return 1 }\nfunc h2() int { return 2 }\nvar A, _ = h1(), h2()\n";
    assert_holds(&p, "t.go", src, "A", "h1");
    assert_not_holds(&p, "t.go", src, "A", "h2");
}

#[test]
fn test_build_go_multi_value_call_for_two_names_gets_first_name_holder() {
    let p = GoProvider::new().unwrap();
    let src = "package p\nfunc load() (int, error) { return 1, nil }\nvar cfg, _ = load()\n";
    assert_holds(&p, "t.go", src, "cfg", "load");
}

#[test]
fn test_build_java_shared_static_field_statement_call_gets_own_declarator() {
    let p = JavaProvider::new().unwrap();
    let src = "class T { static int f() { return 1; } static int g() { return 2; } static int A = f(), B = g(); }";
    assert_holds(&p, "T.java", src, "A", "f");
    assert_holds(&p, "T.java", src, "B", "g");
    assert_not_holds(&p, "T.java", src, "A", "g");
}

#[test]
fn test_build_cpp_lone_member_initializer_next_to_multi_declarator_member_gets_own_holder() {
    let p = CppProvider::new().unwrap();
    let src = "int f() { return 1; }\nstruct T { int a = 0, *b = nullptr; int c = f(); };";
    assert_holds(&p, "t.cpp", src, "c", "f");
}

#[test]
fn test_build_java_nested_static_blocks_bind_own_type_member() {
    let p = JavaProvider::new().unwrap();
    let src = "class O { static void target() {} static class N { static void target() {} static { target(); } } }";
    let g = graph(&p, &[("O.java", src)]);
    assert_eq!(callee_owners(&g, "N"), vec!["N".to_string()]);
}

#[test]
fn test_build_kotlin_nested_init_blocks_bind_own_type_member() {
    let p = KotlinProvider::new().unwrap();
    // One line would not do: tree-sitter-kotlin parses a nested class whose
    // braces close on one line to an ERROR node.
    let src = "class O {\n    fun target() {}\n    class N {\n        fun target() {}\n        init { target() }\n    }\n}\n";
    let g = graph(&p, &[("O.kt", src)]);
    assert_eq!(callee_owners(&g, "N"), vec!["N".to_string()]);
}

#[test]
fn test_build_python_class_body_call_binds_member_defined_earlier() {
    let p = PythonProvider::new().unwrap();
    let src = "def target(): pass\nclass T:\n    def target(): pass\n    x = target()\n";
    let g = graph(&p, &[("t.py", src)]);
    assert_eq!(callee_owners(&g, "x"), vec!["T".to_string()]);
}

#[test]
fn test_build_python_class_body_call_ignores_member_defined_later() {
    let p = PythonProvider::new().unwrap();
    let src = "def target(): pass\nclass T:\n    x = target()\n    def target(): pass\n";
    let g = graph(&p, &[("t.py", src)]);
    assert_eq!(callee_owners(&g, "x"), vec![String::new()]);
}

#[test]
fn test_build_java_variant_reference_in_field_initializer_held_by_value() {
    let p = JavaProvider::new().unwrap();
    let src = "enum Color { RED }\nclass T { static Color C = Color.RED; }";
    let g = graph(&p, &[("T.java", src)]);
    assert!(variants_referenced(&g, "C").contains(&"RED".to_string()));
}

#[test]
fn test_build_java_variant_reference_in_static_block_held_by_type() {
    let p = JavaProvider::new().unwrap();
    let src = "enum Color { RED }\nclass T { static { Color c = Color.RED; } }";
    let g = graph(&p, &[("T.java", src)]);
    assert!(variants_referenced(&g, "T").contains(&"RED".to_string()));
}

#[test]
fn test_build_ruby_function_reference_in_class_constant_held_by_value() {
    let p = RubyProvider::new().unwrap();
    let src = "def target; end\nclass T\n  H = method(:target)\nend\n";
    let g = graph(&p, &[("t.rb", src)]);
    assert!(
        function_refs(&g, "H").iter().any(|r| r.1 == "target"),
        "H must hold a function reference to target, got {:?}",
        function_refs(&g, "H")
    );
}

#[test]
fn test_build_rust_module_is_no_holder_for_thread_local_call() {
    let p = RustProvider::new().unwrap();
    let src = "fn helper() {}\n#[cfg(test)]\nmod tests {\n    thread_local! { static X: () = super::helper(); }\n}\n";
    let g = graph(&p, &[("src/lib.rs", src)]);
    assert!(callers_of(&g, "helper")
        .iter()
        .all(|(_, name)| name != "tests"));
}

fn assert_own_declarators(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    names: [&str; 2],
) {
    assert_holds(provider, path, src, names[0], "f");
    assert_holds(provider, path, src, names[1], "g");
    assert_not_holds(provider, path, src, names[0], "g");
    assert_not_holds(provider, path, src, names[1], "f");
}

#[test]
fn test_build_c_file_scope_shared_declaration_call_gets_own_declarator() {
    let src = "int f(void) { return 1; }\nint g(void) { return 2; }\nint a = f(), b = g();\n";
    assert_own_declarators(&CProvider::new().unwrap(), "t.c", src, ["a", "b"]);
}

#[test]
fn test_build_c_bare_and_initialized_declarators_call_gets_initialized_one() {
    let src = "int g(void) { return 2; }\nint a, b = g();\n";
    let p = CProvider::new().unwrap();
    assert_holds(&p, "t.c", src, "b", "g");
    assert_not_holds(&p, "t.c", src, "a", "g");
}

#[test]
fn test_build_cpp_bare_and_initialized_declarators_call_gets_initialized_one() {
    let src = "int g() { return 2; }\nnamespace N { int a, b = g(); }\n";
    let p = CppProvider::new().unwrap();
    assert_holds(&p, "t.cpp", src, "b", "g");
    assert_not_holds(&p, "t.cpp", src, "a", "g");
}

#[test]
fn test_build_cpp_namespace_scope_shared_declaration_call_gets_own_declarator() {
    let src =
        "int f() { return 1; }\nint g() { return 2; }\nnamespace N { int a = f(), b = g(); }\n";
    assert_own_declarators(&CppProvider::new().unwrap(), "t.cpp", src, ["a", "b"]);
}

#[test]
fn test_build_cpp_struct_shared_member_call_gets_own_declarator() {
    let src = "int f() { return 1; }\nint g() { return 2; }\nstruct S { int a = f(), b = g(); };\n";
    assert_own_declarators(&CppProvider::new().unwrap(), "t.cpp", src, ["a", "b"]);
}

#[test]
fn test_build_csharp_shared_static_field_statement_call_gets_own_declarator() {
    let src = "class T { static int f() { return 1; } static int g() { return 2; } static int A = f(), B = g(); }";
    let p = CSharpProvider::new().unwrap();
    assert_holds(&p, "T.cs", src, "A", "f");
    assert_holds(&p, "T.cs", src, "B", "g");
    assert_not_holds(&p, "T.cs", src, "A", "g");
    assert_not_holds(&p, "T.cs", src, "B", "f");
}

#[test]
fn test_build_typescript_shared_const_statement_call_gets_own_declarator() {
    let src = "function f() {}\nfunction g() {}\nconst a = f(), b = g();";
    assert_own_declarators(&TypeScriptProvider::new().unwrap(), "t.ts", src, ["a", "b"]);
}

#[test]
fn test_build_swift_top_level_shared_let_call_gets_own_declarator() {
    let src = "func f() -> Int { 1 }\nfunc g() -> Int { 2 }\nlet a = f(), b = g()\n";
    assert_own_declarators(&SwiftProvider::new().unwrap(), "t.swift", src, ["a", "b"]);
}

#[test]
fn test_build_dart_top_level_shared_final_call_gets_own_declarator() {
    let src = "int f() => 1;\nint g() => 2;\nfinal a = f(), b = g();\n";
    assert_own_declarators(&DartProvider::new().unwrap(), "t.dart", src, ["a", "b"]);
}

#[test]
fn test_build_dart_class_static_shared_final_call_gets_own_declarator() {
    let src = "int f() => 1;\nint g() => 2;\nclass T { static final a = f(), b = g(); }\n";
    assert_own_declarators(&DartProvider::new().unwrap(), "t.dart", src, ["a", "b"]);
}

#[test]
fn test_build_ruby_module_body_call_gets_holder() {
    assert_holds(
        &RubyProvider::new().unwrap(),
        "t.rb",
        "def target(name); end\nmodule M\n  target :x\nend\n",
        "M",
        "target",
    );
}

#[test]
fn test_build_java_enum_static_block_call_gets_holder() {
    assert_holds(
        &JavaProvider::new().unwrap(),
        "E.java",
        "enum E { A; static void target() {} static { target(); } }",
        "E",
        "target",
    );
}

#[test]
fn test_build_java_static_block_bare_field_read_gets_no_class_holder() {
    let p = JavaProvider::new().unwrap();
    let g = graph(
        &p,
        &[(
            "T.java",
            "class T { static int F = 1; static { int x = F; } }",
        )],
    );
    assert_eq!(value_reads(&g, "T"), vec![]);
}

#[test]
fn test_build_php_global_const_new_initializer_gets_constructor_holder() {
    let p = PhpProvider::new().unwrap();
    let src = "<?php\nclass Foo { public function __construct() {} }\nconst X = new Foo();\n";
    let g = graph(&p, &[("t.php", src)]);
    let got = calls(&g, "X");
    assert!(
        got.iter().any(|(_, n)| n == "__construct" || n == "Foo"),
        "X must hold the Foo constructor call, got {got:?}"
    );
}
