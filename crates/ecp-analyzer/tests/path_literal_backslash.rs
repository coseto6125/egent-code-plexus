//! Cross-language regression: backslash strings that are PHP namespaces or
//! regexes must not become `PathLiteral`s, while real Windows paths still do
//! (FU-2026-10-22-912dc51dcfc2). The literal value is the raw source text, so
//! each source holds the escaped form (`\\` = one runtime backslash).

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
use ecp_core::analyzer::types::RawPathLiteral;
use std::path::Path;

fn path_literals<P: LanguageProvider>(provider: P, file: &str, src: &str) -> Vec<RawPathLiteral> {
    provider
        .parse_file(Path::new(file), src.as_bytes())
        .expect("parse_file")
        .path_literals
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

#[test]
fn test_is_path_shaped_typescript_namespace_rejected() {
    let src = r##"import fs from "fs";
function f() { return fs.readFileSync("App\\Models\\User"); }
"##;
    let lits = path_literals(TypeScriptProvider::new().expect("provider"), "test.ts", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_typescript_regex_rejected() {
    let src = r##"import fs from "fs";
function f() { return fs.readFileSync("\\d+\\.\\d+"); }
"##;
    let lits = path_literals(TypeScriptProvider::new().expect("provider"), "test.ts", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_typescript_windows_path_kept() {
    let src = r##"import fs from "fs";
function f() { return fs.readFileSync("C:\\Users\\x\\file.txt"); }
"##;
    let lits = path_literals(TypeScriptProvider::new().expect("provider"), "test.ts", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_javascript_namespace_rejected() {
    let src = r##"const fs = require("fs");
function f() { return fs.readFileSync("App\\Models\\User"); }
"##;
    let lits = path_literals(JavaScriptProvider::new().expect("provider"), "test.js", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_javascript_regex_rejected() {
    let src = r##"const fs = require("fs");
function f() { return fs.readFileSync("\\d+\\.\\d+"); }
"##;
    let lits = path_literals(JavaScriptProvider::new().expect("provider"), "test.js", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_javascript_windows_path_kept() {
    let src = r##"const fs = require("fs");
function f() { return fs.readFileSync("C:\\Users\\x\\file.txt"); }
"##;
    let lits = path_literals(JavaScriptProvider::new().expect("provider"), "test.js", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_python_namespace_rejected() {
    let src = r##"def f():
    return open("App\\Models\\User")
"##;
    let lits = path_literals(PythonProvider::new().expect("provider"), "test.py", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_python_regex_rejected() {
    let src = r##"def f():
    return open("\\d+\\.\\d+")
"##;
    let lits = path_literals(PythonProvider::new().expect("provider"), "test.py", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_python_windows_path_kept() {
    let src = r##"def f():
    return open("C:\\Users\\x\\file.txt")
"##;
    let lits = path_literals(PythonProvider::new().expect("provider"), "test.py", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_java_namespace_rejected() {
    let src = r##"class T { void f() { new java.io.File("App\\Models\\User"); } }
"##;
    let lits = path_literals(JavaProvider::new().expect("provider"), "Test.java", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_java_regex_rejected() {
    let src = r##"class T { void f() { new java.io.File("\\d+\\.\\d+"); } }
"##;
    let lits = path_literals(JavaProvider::new().expect("provider"), "Test.java", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_java_windows_path_kept() {
    let src = r##"class T { void f() { new java.io.File("C:\\Users\\x\\file.txt"); } }
"##;
    let lits = path_literals(JavaProvider::new().expect("provider"), "Test.java", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_kotlin_namespace_rejected() {
    let src = r##"fun f(): String = File("App\\Models\\User").readText()
"##;
    let lits = path_literals(KotlinProvider::new().expect("provider"), "Test.kt", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_kotlin_regex_rejected() {
    let src = r##"fun f(): String = File("\\d+\\.\\d+").readText()
"##;
    let lits = path_literals(KotlinProvider::new().expect("provider"), "Test.kt", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_kotlin_windows_path_kept() {
    let src = r##"fun f(): String = File("C:\\Users\\x\\file.txt").readText()
"##;
    let lits = path_literals(KotlinProvider::new().expect("provider"), "Test.kt", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_csharp_namespace_rejected() {
    let src = r##"class T { void F() { System.IO.File.ReadAllText("App\\Models\\User"); } }
"##;
    let lits = path_literals(CSharpProvider::new().expect("provider"), "Test.cs", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_csharp_regex_rejected() {
    let src = r##"class T { void F() { System.IO.File.ReadAllText("\\d+\\.\\d+"); } }
"##;
    let lits = path_literals(CSharpProvider::new().expect("provider"), "Test.cs", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_csharp_windows_path_kept() {
    let src = r##"class T { void F() { System.IO.File.ReadAllText("C:\\Users\\x\\file.txt"); } }
"##;
    let lits = path_literals(CSharpProvider::new().expect("provider"), "Test.cs", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_go_namespace_rejected() {
    let src = r##"package main
import "os"
func f() { os.ReadFile("App\\Models\\User") }
"##;
    let lits = path_literals(GoProvider::new().expect("provider"), "test.go", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_go_regex_rejected() {
    let src = r##"package main
import "os"
func f() { os.ReadFile("\\d+\\.\\d+") }
"##;
    let lits = path_literals(GoProvider::new().expect("provider"), "test.go", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_go_windows_path_kept() {
    let src = r##"package main
import "os"
func f() { os.ReadFile("C:\\Users\\x\\file.txt") }
"##;
    let lits = path_literals(GoProvider::new().expect("provider"), "test.go", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_rust_namespace_rejected() {
    let src = r##"fn f() { let _ = std::fs::read_to_string("App\\Models\\User"); }
"##;
    let lits = path_literals(RustProvider::new().expect("provider"), "test.rs", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_rust_regex_rejected() {
    let src = r##"fn f() { let _ = std::fs::read_to_string("\\d+\\.\\d+"); }
"##;
    let lits = path_literals(RustProvider::new().expect("provider"), "test.rs", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_rust_windows_path_kept() {
    let src = r##"fn f() { let _ = std::fs::read_to_string("C:\\Users\\x\\file.txt"); }
"##;
    let lits = path_literals(RustProvider::new().expect("provider"), "test.rs", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_php_namespace_rejected() {
    let src = r##"<?php
function f() { return file_get_contents("App\\Models\\User"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_php_regex_rejected() {
    let src = r##"<?php
function f() { return file_get_contents("\\d+\\.\\d+"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_php_windows_path_kept() {
    let src = r##"<?php
function f() { return file_get_contents("C:\\Users\\x\\file.txt"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_ruby_namespace_rejected() {
    let src = r##"def f
  File.read("App\\Models\\User")
end
"##;
    let lits = path_literals(RubyProvider::new().expect("provider"), "test.rb", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_ruby_regex_rejected() {
    let src = r##"def f
  File.read("\\d+\\.\\d+")
end
"##;
    let lits = path_literals(RubyProvider::new().expect("provider"), "test.rb", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_ruby_windows_path_kept() {
    let src = r##"def f
  File.read("C:\\Users\\x\\file.txt")
end
"##;
    let lits = path_literals(RubyProvider::new().expect("provider"), "test.rb", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_swift_namespace_rejected() {
    let src = r##"func f() { let _ = try? String(contentsOfFile: "App\\Models\\User") }
"##;
    let lits = path_literals(SwiftProvider::new().expect("provider"), "test.swift", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_swift_regex_rejected() {
    let src = r##"func f() { let _ = try? String(contentsOfFile: "\\d+\\.\\d+") }
"##;
    let lits = path_literals(SwiftProvider::new().expect("provider"), "test.swift", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_swift_windows_path_kept() {
    let src = r##"func f() { let _ = try? String(contentsOfFile: "C:\\Users\\x\\file.txt") }
"##;
    let lits = path_literals(SwiftProvider::new().expect("provider"), "test.swift", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_c_namespace_rejected() {
    let src = r##"#include <stdio.h>
void f(void) { fopen("App\\Models\\User", "r"); }
"##;
    let lits = path_literals(CProvider::new().expect("provider"), "test.c", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_c_regex_rejected() {
    let src = r##"#include <stdio.h>
void f(void) { fopen("\\d+\\.\\d+", "r"); }
"##;
    let lits = path_literals(CProvider::new().expect("provider"), "test.c", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_c_windows_path_kept() {
    let src = r##"#include <stdio.h>
void f(void) { fopen("C:\\Users\\x\\file.txt", "r"); }
"##;
    let lits = path_literals(CProvider::new().expect("provider"), "test.c", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_cpp_namespace_rejected() {
    let src = r##"#include <fstream>
void f() { std::ifstream in("App\\Models\\User"); }
"##;
    let lits = path_literals(CppProvider::new().expect("provider"), "test.cpp", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_cpp_regex_rejected() {
    let src = r##"#include <fstream>
void f() { std::ifstream in("\\d+\\.\\d+"); }
"##;
    let lits = path_literals(CppProvider::new().expect("provider"), "test.cpp", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_cpp_windows_path_kept() {
    let src = r##"#include <fstream>
void f() { std::ifstream in("C:\\Users\\x\\file.txt"); }
"##;
    let lits = path_literals(CppProvider::new().expect("provider"), "test.cpp", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_dart_namespace_rejected() {
    let src = r##"void f() { File("App\\Models\\User").readAsStringSync(); }
"##;
    let lits = path_literals(DartProvider::new().expect("provider"), "test.dart", src);
    assert!(
        !lits.iter().any(|l| l.value == r"App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_dart_regex_rejected() {
    let src = r##"void f() { File("\\d+\\.\\d+").readAsStringSync(); }
"##;
    let lits = path_literals(DartProvider::new().expect("provider"), "test.dart", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\d+\\.\\d+"),
        "false PathLiteral surfaced: {lits:?}"
    );
}

#[test]
fn test_is_path_shaped_dart_windows_path_kept() {
    let src = r##"void f() { File("C:\\Users\\x\\file.txt").readAsStringSync(); }
"##;
    let lits = path_literals(DartProvider::new().expect("provider"), "test.dart", src);
    assert!(
        lits.iter().any(|l| l.value == r"C:\\Users\\x\\file.txt"),
        "real Windows path must surface: {lits:?}"
    );
}

#[test]
fn test_extract_string_content_php_member_interpolation_skipped() {
    let src = r##"<?php
function f($d) { return file_get_contents("logs/{$this->dir}/app.log") . file_get_contents("logs/{$d['k']}/app.log") . file_get_contents("logs/${d}/app.log"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    assert!(
        !lits
            .iter()
            .any(|l| l.value.contains("$this->") || l.value.contains("$d")),
        "interpolated expression leaked into a value: {lits:?}"
    );
}

#[test]
fn test_extract_string_content_php_plain_and_escaped_unchanged() {
    let src = r##"<?php
function f() { return file_get_contents("storage/app.log") . file_get_contents("C:\\logs\\app.log"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    for want in ["storage/app.log", r"C:\\logs\\app.log"] {
        assert!(lits.iter().any(|l| l.value == want), "{want}: {lits:?}");
    }
}

#[test]
fn test_is_path_shaped_php_escaped_fqcn_with_leading_backslash_rejected() {
    let src = r##"<?php
function f() { return file_get_contents("\\App\\Models\\User"); }
"##;
    let lits = path_literals(PhpProvider::new().expect("provider"), "test.php", src);
    assert!(
        !lits.iter().any(|l| l.value == r"\\App\\Models\\User"),
        "false PathLiteral surfaced: {lits:?}"
    );
}
