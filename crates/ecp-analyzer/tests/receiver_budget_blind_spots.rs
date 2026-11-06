//! A function whose receiver analysis exceeds the solver budget surfaces a
//! `receiver-budget` blind spot, quoting the real file path, in every language
//! that lowers locals into the receiver core.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::receiver::RECEIVER_BUDGET_KIND;
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::fmt::Write;
use std::path::Path;

const OVER_BUDGET_LOCALS: usize = 300;
const UNDER_BUDGET_LOCALS: usize = 10;

fn lines(count: usize, line: impl Fn(usize) -> String) -> String {
    (0..count).fold(String::new(), |mut out, i| {
        writeln!(out, "{}", line(i)).unwrap();
        out
    })
}

struct Case {
    path: &'static str,
    source: fn(usize) -> String,
    parse: fn(&Path, &[u8]) -> Vec<(String, std::path::PathBuf)>,
}

fn budget_spots<P: LanguageProvider>(
    provider: &P,
    path: &Path,
    source: &[u8],
) -> Vec<(String, std::path::PathBuf)> {
    provider
        .parse_file(path, source)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
        .blind_spots
        .into_iter()
        .filter(|spot| spot.kind == RECEIVER_BUDGET_KIND)
        .map(|spot| (spot.kind, spot.file_path))
        .collect()
}

fn python(n: usize) -> String {
    let body = lines(n, |i| format!("    a{i} = A()\n    a{i}.m()"));
    format!("class A:\n    def m(self):\n        pass\n\ndef f():\n{body}")
}

fn ruby(n: usize) -> String {
    let body = lines(n, |i| format!("  a{i} = A.new\n  a{i}.m"));
    format!("class A\n  def m\n  end\nend\n\ndef f\n{body}end\n")
}

fn php(n: usize) -> String {
    let body = lines(n, |i| format!("    $a{i} = new A();\n    $a{i}->m();"));
    format!("<?php\nclass A {{ function m() {{}} }}\nfunction f() {{\n{body}}}\n")
}

fn javascript(n: usize) -> String {
    let body = lines(n, |i| format!("  const a{i} = new A();\n  a{i}.m();"));
    format!("class A {{ m() {{}} }}\nfunction f() {{\n{body}}}\n")
}

fn typescript(n: usize) -> String {
    let body = lines(n, |i| format!("  const a{i}: A = new A();\n  a{i}.m();"));
    format!("class A {{ m(): void {{}} }}\nfunction f(): void {{\n{body}}}\n")
}

fn java(n: usize) -> String {
    let body = lines(n, |i| format!("    A a{i} = new A();\n    a{i}.m();"));
    format!("class A {{ void m() {{}} }}\nclass T {{\n  void f() {{\n{body}  }}\n}}\n")
}

fn csharp(n: usize) -> String {
    let body = lines(n, |i| format!("    A a{i} = new A();\n    a{i}.M();"));
    format!("class A {{ public void M() {{}} }}\nclass T {{\n  void F() {{\n{body}  }}\n}}\n")
}

fn kotlin(n: usize) -> String {
    let body = lines(n, |i| format!("    val a{i}: A = A()\n    a{i}.m()"));
    format!("class A {{ fun m() {{}} }}\n\nfun f() {{\n{body}}}\n")
}

fn cpp(n: usize) -> String {
    let body = lines(n, |i| format!("    A a{i};\n    a{i}.m();"));
    format!("struct A {{ void m() {{}} }};\n\nvoid f() {{\n{body}}}\n")
}

fn swift(n: usize) -> String {
    let body = lines(n, |i| format!("    let a{i}: A = A()\n    a{i}.m()"));
    format!("class A {{ func m() {{}} }}\n\nfunc f() {{\n{body}}}\n")
}

fn go(n: usize) -> String {
    let body = lines(n, |i| format!("\tvar a{i} A\n\ta{i}.M()"));
    format!("package p\n\ntype A struct{{}}\n\nfunc (A) M() {{}}\n\nfunc f() {{\n{body}}}\n")
}

macro_rules! case {
    ($path:literal, $source:ident, $provider:expr) => {
        Case {
            path: $path,
            source: $source,
            parse: |path, source| budget_spots(&$provider.expect("provider"), path, source),
        }
    };
}

#[test]
fn test_parse_file_function_over_receiver_budget_emits_blind_spot_across_languages() {
    let cases = [
        case!("src/budget.py", python, PythonProvider::new()),
        case!("src/budget.php", php, PhpProvider::new()),
        case!("src/budget.rb", ruby, RubyProvider::new()),
        case!("src/budget.js", javascript, JavaScriptProvider::new()),
        case!("src/budget.ts", typescript, TypeScriptProvider::new()),
        case!("src/Budget.java", java, JavaProvider::new()),
        case!("src/Budget.cs", csharp, CSharpProvider::new()),
        case!("src/Budget.kt", kotlin, KotlinProvider::new()),
        case!("src/budget.cpp", cpp, CppProvider::new()),
        case!("src/Budget.swift", swift, SwiftProvider::new()),
        case!("src/budget.go", go, GoProvider::new()),
    ];
    for case in cases {
        let path = Path::new(case.path);
        let over = (case.parse)(path, (case.source)(OVER_BUDGET_LOCALS).as_bytes());
        assert_eq!(
            over,
            vec![(RECEIVER_BUDGET_KIND.to_owned(), path.to_path_buf())],
            "{}: {OVER_BUDGET_LOCALS} typed locals must emit exactly one receiver-budget spot",
            case.path
        );
        let under = (case.parse)(path, (case.source)(UNDER_BUDGET_LOCALS).as_bytes());
        assert!(
            under.is_empty(),
            "{}: {UNDER_BUDGET_LOCALS} typed locals must emit none, got {under:?}",
            case.path
        );
    }
}
