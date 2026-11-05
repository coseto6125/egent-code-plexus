//! `HasMethod` says "this type owns this method". A module, namespace or
//! package never owns a free function through it, a closure is never a
//! member, and the edge's reason names the pass that produced it, not Rust
//! in every language.
//!
//! Stands for the contract: `MATCH (t)-[r:HasMethod]->(m)` returns only
//! type-to-member rows, and `r.reason` is `post_process:class_membership`
//! (the type's span holds the member), `...:rust_impl` (a Rust `impl` block)
//! or `...:owner` (the parser named the type: Go receiver, Swift extension,
//! C++ out-of-line `Cls::m`).
//!
//! A language with no free function that a module could claim (Java, C#,
//! Ruby, C) pins the real method's edge and the absent module or closure edge
//! only. A Ruby `module` is a mixin that owns its instance methods, so
//! `Util -> helper` is a real member edge there.

use crate::name_ref_support::graph;

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

const SPAN: &str = "post_process:class_membership";
const IMPL: &str = "post_process:class_membership:rust_impl";
const OWNER: &str = "post_process:class_membership:owner";

/// Every `HasMethod` edge as `(owner, member, reason)`, sorted.
fn has_method(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
) -> Vec<(String, String, String)> {
    let g = graph(provider, &[(path, src)]);
    let pool = g.string_pool.as_slice();
    let mut out: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::HasMethod)
        .map(|e| {
            (
                g.nodes[e.source as usize].name.resolve(pool).to_string(),
                g.nodes[e.target as usize].name.resolve(pool).to_string(),
                e.reason.resolve(pool).to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

fn edge(owner: &str, member: &str, reason: &str) -> (String, String, String) {
    (owner.into(), member.into(), reason.into())
}

/// Asserts the real method keeps its edge with `reason`, no free function in
/// `free` is a member, and no closure is a member of anything.
fn assert_owner(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    kept: (&str, &str, &str),
    free: &[&str],
) {
    let edges = has_method(provider, path, src);
    assert!(
        edges.contains(&edge(kept.0, kept.1, kept.2)),
        "{path}: missing {kept:?} in {edges:?}"
    );
    for (owner, member, reason) in &edges {
        assert!(
            !free.contains(&member.as_str()),
            "{path}: `{owner}` claims free function `{member}` ({reason}) in {edges:?}"
        );
        assert!(
            !member.starts_with("<anonymous") && !member.is_empty(),
            "{path}: closure `{member}` is a member of `{owner}` in {edges:?}"
        );
        if reason == IMPL {
            assert!(path.ends_with(".rs"), "{path}: `{reason}` outside Rust");
        }
    }
}

#[test]
fn test_has_method_rust_mod_function_has_no_owner_and_impl_keeps_edge() {
    let src = "pub mod parse { pub fn cb_qual() {} }\n\
               pub struct S;\n\
               impl S { pub fn m(&self) { let c = || 1; c(); } }\n\
               pub trait T { fn t(&self); }\n\
               impl T for S { fn t(&self) {} }\n";
    let p = RustProvider::new().unwrap();
    assert_owner(&p, "src/lib.rs", src, ("S", "m", IMPL), &["cb_qual"]);
    let edges = has_method(&p, "src/lib.rs", src);
    assert!(edges.contains(&edge("S", "t", IMPL)), "{edges:?}");
    assert!(edges.contains(&edge("T", "t", SPAN)), "{edges:?}");
}

#[test]
fn test_has_method_rust_module_alone_emits_nothing() {
    let src = "pub mod parse { pub fn a() {} pub fn b() {} }\n";
    let p = RustProvider::new().unwrap();
    assert_eq!(has_method(&p, "src/lib.rs", src), vec![]);
}

#[test]
fn test_has_method_rust_same_input_twice_gives_the_same_edges() {
    let src = "pub struct S;\nimpl S { pub fn m(&self) {} }\n";
    let p = RustProvider::new().unwrap();
    assert_eq!(
        has_method(&p, "src/lib.rs", src),
        has_method(&p, "src/lib.rs", src)
    );
}

#[test]
fn test_has_method_cpp_namespace_function_has_no_owner_and_out_of_line_keeps_edge() {
    let src = "namespace ns { void free_fn() {} }\n\
               struct Cls { void out(); };\n\
               void Cls::out() {}\n";
    let p = CppProvider::new().unwrap();
    assert_owner(&p, "src/a.cpp", src, ("Cls", "out", OWNER), &["free_fn"]);
}

#[test]
fn test_has_method_cpp_namespace_alone_emits_nothing() {
    let p = CppProvider::new().unwrap();
    let src = "namespace ns { void free_fn() {} }\n";
    assert_eq!(has_method(&p, "src/a.cpp", src), vec![]);
}

#[test]
fn test_has_method_go_receiver_method_keeps_edge_with_owner_reason() {
    let src = "package main\n\
               type T struct{}\n\
               func (t T) Meth() {}\n\
               func Free() {}\n";
    let p = GoProvider::new().unwrap();
    assert_owner(&p, "main.go", src, ("T", "Meth", OWNER), &["Free"]);
}

#[test]
fn test_has_method_go_receiver_of_a_type_declared_elsewhere_emits_nothing() {
    // The type is in another file: the pass binds only a type row of this file.
    let src = "package main\nfunc (t Other) Meth() {}\n";
    let p = GoProvider::new().unwrap();
    assert_eq!(has_method(&p, "main.go", src), vec![]);
}

#[test]
fn test_has_method_swift_extension_member_keeps_edge_with_owner_reason() {
    let src = "struct Cb {}\n\
               extension Cb { func ext() {} }\n\
               func freeS() {}\n";
    let p = SwiftProvider::new().unwrap();
    assert_owner(&p, "Main.swift", src, ("Cb", "ext", OWNER), &["freeS"]);
}

#[test]
fn test_has_method_typescript_namespace_function_has_no_owner_and_class_keeps_edge() {
    let src = "namespace N { export function nf() {} }\n\
               class C { m() { const f = () => 1; return f(); } }\n\
               function top() { return () => 2; }\n";
    let p = TypeScriptProvider::new().unwrap();
    assert_owner(&p, "src/a.ts", src, ("C", "m", SPAN), &["nf", "top"]);
}

#[test]
fn test_has_method_javascript_class_keeps_edge_and_closure_is_no_member() {
    let src = "class C { m() { const f = () => 1; return f(); } }\n\
               function top() { return () => 2; }\n";
    let p = JavaScriptProvider::new().unwrap();
    assert_owner(&p, "src/a.js", src, ("C", "m", SPAN), &["top"]);
}

#[test]
fn test_has_method_python_module_function_has_no_owner_and_class_keeps_edge() {
    let src =
        "def free():\n    def inner():\n        pass\n\nclass Q:\n    def m(self):\n        pass\n";
    let p = PythonProvider::new().unwrap();
    assert_owner(&p, "a.py", src, ("Q", "m", SPAN), &["free", "inner"]);
}

#[test]
fn test_has_method_java_class_keeps_edge_and_lambda_is_no_member() {
    let src = "class K { void m() { Runnable r = () -> {}; r.run(); } }\n";
    let p = JavaProvider::new().unwrap();
    assert_owner(&p, "K.java", src, ("K", "m", SPAN), &[]);
}

#[test]
fn test_has_method_kotlin_top_level_function_has_no_owner_and_class_keeps_edge() {
    let src = "package p\nfun freeK() {}\nclass K { fun m() { val f = { 1 }; f() } }\n";
    let p = KotlinProvider::new().unwrap();
    assert_owner(&p, "a.kt", src, ("K", "m", SPAN), &["freeK"]);
}

#[test]
fn test_has_method_csharp_namespace_owns_no_type_member_and_class_keeps_edge() {
    let src = "namespace NS { class K { void M() { System.Action a = () => {}; a(); } } }\n";
    let p = CSharpProvider::new().unwrap();
    assert_owner(&p, "K.cs", src, ("K", "M", SPAN), &[]);
    let edges = has_method(&p, "K.cs", src);
    assert!(edges.iter().all(|(owner, ..)| owner != "NS"), "{edges:?}");
}

#[test]
fn test_has_method_php_namespace_function_has_no_owner_and_class_keeps_edge() {
    let src = "<?php\nnamespace App;\nfunction free_fn() {}\nclass P { function m() {} }\n";
    let p = PhpProvider::new().unwrap();
    assert_owner(&p, "a.php", src, ("P", "m", SPAN), &["free_fn"]);
}

#[test]
fn test_has_method_ruby_module_and_class_keep_their_member_edges() {
    let src = "module Util\n  def helper; end\nend\nclass C\n  def m; end\nend\n";
    let p = RubyProvider::new().unwrap();
    assert_owner(&p, "a.rb", src, ("C", "m", SPAN), &[]);
    let edges = has_method(&p, "a.rb", src);
    assert!(edges.contains(&edge("Util", "helper", SPAN)), "{edges:?}");
}

#[test]
fn test_has_method_dart_top_level_function_has_no_owner_and_extension_keeps_edge() {
    let src = "void freeD() {}\nclass D { void m() {} }\nextension E on D { void ex() {} }\n";
    let p = DartProvider::new().unwrap();
    assert_owner(&p, "a.dart", src, ("D", "m", SPAN), &["freeD"]);
    let edges = has_method(&p, "a.dart", src);
    assert!(edges.contains(&edge("E", "ex", SPAN)), "{edges:?}");
}

#[test]
fn test_has_method_c_free_function_has_no_owner() {
    // C has no method syntax: a struct with a function-pointer field owns no
    // function through `HasMethod`.
    let src = "struct ops { int (*open)(void); };\nstatic int my_open(void) { return 0; }\n";
    let p = CProvider::new().unwrap();
    assert_eq!(has_method(&p, "a.c", src), vec![]);
}

#[test]
fn test_has_method_empty_and_whitespace_sources_emit_nothing() {
    let p = RustProvider::new().unwrap();
    assert_eq!(has_method(&p, "src/lib.rs", ""), vec![]);
    assert_eq!(has_method(&p, "src/lib.rs", "  \n\t\n"), vec![]);
}
