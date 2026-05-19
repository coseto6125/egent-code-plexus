//! 14-lang grammar shape canary.
//!
//! Round 76 (PR #149) discovered that bumping tree-sitter-swift from
//! 0.21 → 0.25 silently changed the ERROR-recovery shape for `#if`
//! guarded class headers, so cgn-rs stopped emitting those classes
//! without any test failing. The other 13 mainstream langs had no
//! equivalent canary, meaning the next vendor grammar bump could ship
//! the same regression for any of them.
//!
//! This file pins a minimal AST shape per lang: a top-level function,
//! a class-like declaration, and (when the lang has it) a const-style
//! binding. The assertions verify the `(kind, name)` tuples cgn-rs
//! emits — if a grammar bump renames a node (`class_declaration` →
//! `class_definition`) or changes the field name (`name:` → `id:`),
//! the relevant test fails immediately rather than masking a parity
//! regression behind "we just emit fewer things now."
//!
//! Each fixture is intentionally small and uses canonical idioms so
//! the file doubles as documentation for what cgn considers a
//! "minimum recognizable program" per lang. Edge-case parsing
//! (decorators, generics, lambdas, etc.) belongs in the dedicated
//! per-feature test files.

use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

/// Assert every `(kind, name)` in `expected` shows up at least once in
/// the parsed graph. Repeats are ignored — we want shape stability,
/// not duplicate-count stability (duplicates often differ legitimately
/// between grammar versions; node names don't).
fn assert_has(g: &LocalGraph, expected: &[(NodeKind, &str)], label: &str) {
    for (kind, name) in expected {
        let found = g
            .nodes
            .iter()
            .any(|n| n.kind == *kind && n.name == *name);
        assert!(
            found,
            "[{label}] grammar drift: missing ({kind:?}, {name:?}) in:\n{:#?}",
            g.nodes.iter().map(|n| (n.kind, n.name.clone())).collect::<Vec<_>>(),
        );
    }
}

// -- TypeScript ---------------------------------------------------------------

#[test]
fn typescript_shape_pin() {
    use cgn_analyzer::typescript::parser::TypeScriptProvider;
    let src = "\
export function add(a: number, b: number): number { return a + b; }\n\
export class Box<T> { value: T; constructor(v: T) { this.value = v; } }\n\
export const PI = 3.14;\n";
    let p = TypeScriptProvider::new().expect("provider");
    let g = p.parse_file(Path::new("t.ts"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Function, "add"), (NodeKind::Class, "Box")],
        "TypeScript",
    );
}

// -- JavaScript ---------------------------------------------------------------

#[test]
fn javascript_shape_pin() {
    use cgn_analyzer::javascript::parser::JavaScriptProvider;
    let src = "\
export function add(a, b) { return a + b; }\n\
export class Box { constructor(v) { this.value = v; } }\n\
export const PI = 3.14;\n";
    let p = JavaScriptProvider::new().expect("provider");
    let g = p.parse_file(Path::new("t.js"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Function, "add"), (NodeKind::Class, "Box")],
        "JavaScript",
    );
}

// -- Python -------------------------------------------------------------------

#[test]
fn python_shape_pin() {
    use cgn_analyzer::python::parser::PythonProvider;
    let src = "\
def add(a, b):\n    return a + b\n\n\
class Box:\n    def __init__(self, v):\n        self.value = v\n\n\
PI = 3.14\n";
    let p = PythonProvider::new().expect("provider");
    let g = p.parse_file(Path::new("t.py"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Function, "add"), (NodeKind::Class, "Box")],
        "Python",
    );
}

// -- Java ---------------------------------------------------------------------

#[test]
fn java_shape_pin() {
    use cgn_analyzer::java::parser::JavaProvider;
    let src = "\
package demo;\n\
public class Box {\n    public int add(int a, int b) { return a + b; }\n}\n";
    let p = JavaProvider::new().expect("provider");
    let g = p.parse_file(Path::new("Box.java"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Method, "add")],
        "Java",
    );
}

// -- Kotlin -------------------------------------------------------------------

#[test]
fn kotlin_shape_pin() {
    use cgn_analyzer::kotlin::parser::KotlinProvider;
    let src = "\
package demo\n\n\
class Box(val value: Int) {\n    fun add(a: Int, b: Int): Int = a + b\n}\n\n\
fun pi(): Double = 3.14\n";
    let p = KotlinProvider::new().expect("provider");
    let g = p.parse_file(Path::new("t.kt"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Function, "pi")],
        "Kotlin",
    );
}

// -- C# -----------------------------------------------------------------------

#[test]
fn csharp_shape_pin() {
    use cgn_analyzer::c_sharp::parser::CSharpProvider;
    let src = "\
namespace Demo {\n  public class Box {\n    public int Add(int a, int b) { return a + b; }\n  }\n}\n";
    let p = CSharpProvider::new().expect("provider");
    let g = p.parse_file(Path::new("Box.cs"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Method, "Add")],
        "CSharp",
    );
}

// -- Go -----------------------------------------------------------------------

#[test]
fn go_shape_pin() {
    use cgn_analyzer::go::parser::GoProvider;
    let src = "\
package demo\n\n\
type Box struct { Value int }\n\n\
func (b *Box) Add(a, c int) int { return a + c }\n\n\
const PI = 3.14\n";
    let p = GoProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.go"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[
            (NodeKind::Struct, "Box"),
            (NodeKind::Method, "Add"),
            (NodeKind::Const, "PI"),
        ],
        "Go",
    );
}

// -- Rust ---------------------------------------------------------------------

#[test]
fn rust_shape_pin() {
    use cgn_analyzer::rust::parser::RustProvider;
    let src = "\
pub struct Box { value: i32 }\n\n\
impl Box {\n    pub fn add(&self, a: i32, b: i32) -> i32 { a + b }\n}\n\n\
pub fn pi() -> f64 { 3.14 }\n";
    let p = RustProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.rs"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Struct, "Box"), (NodeKind::Function, "pi")],
        "Rust",
    );
}

// -- PHP ----------------------------------------------------------------------

#[test]
fn php_shape_pin() {
    use cgn_analyzer::php::parser::PhpProvider;
    let src = "<?php\nnamespace Demo;\n\nclass Box {\n    public function add(int $a, int $b): int { return $a + $b; }\n}\n";
    let p = PhpProvider::new().expect("provider");
    let g = p.parse_file(Path::new("Box.php"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Method, "add")],
        "PHP",
    );
}

// -- Ruby ---------------------------------------------------------------------

#[test]
fn ruby_shape_pin() {
    use cgn_analyzer::ruby::parser::RubyProvider;
    let src = "\
class Box\n  def add(a, b)\n    a + b\n  end\nend\n\n\
def pi\n  3.14\nend\n";
    let p = RubyProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.rb"), src.as_bytes()).expect("parse");
    // Ruby `def` is always a Method in cgn-rs's taxonomy — Ruby has no
    // "free function" concept (top-level `def` binds to Kernel /
    // main:Object), so both inside-class and top-level `def` emit Method.
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Method, "pi")],
        "Ruby",
    );
}

// -- Swift --------------------------------------------------------------------

#[test]
fn swift_shape_pin() {
    use cgn_analyzer::swift::parser::SwiftProvider;
    // The `#if ... #endif` guard is the exact shape the Round 76
    // tree-sitter-swift 0.21 → 0.25 bump silently broke. Pinning it
    // here means any future Swift grammar bump that re-breaks ERROR
    // recovery around `#if` headers shows up as a test failure
    // instead of as a parity regression.
    let src = "\
#if canImport(Foundation)\n\
import Foundation\n\
#endif\n\n\
class Box {\n    func add(_ a: Int, _ b: Int) -> Int { return a + b }\n}\n\n\
func pi() -> Double { return 3.14 }\n";
    let p = SwiftProvider::new().expect("provider");
    let g = p.parse_file(Path::new("Box.swift"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Function, "pi")],
        "Swift",
    );
}

// -- C ------------------------------------------------------------------------

#[test]
fn c_shape_pin() {
    use cgn_analyzer::c::parser::CProvider;
    let src = "\
#include <stdio.h>\n\n\
typedef struct { int value; } Box;\n\n\
int add(int a, int b) { return a + b; }\n\n\
#define PI 3.14\n";
    let p = CProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.c"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[
            (NodeKind::Function, "add"),
            (NodeKind::Typedef, "Box"),
            (NodeKind::Macro, "PI"),
        ],
        "C",
    );
}

// -- C++ ----------------------------------------------------------------------

#[test]
fn cpp_shape_pin() {
    use cgn_analyzer::cpp::parser::CppProvider;
    let src = "\
#include <iostream>\n\n\
namespace demo {\n\n\
class Box { public: int add(int a, int b) { return a + b; } };\n\n\
union Tag { int i; float f; };\n\n\
typedef int IntAlias;\n\n\
}\n";
    let p = CppProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.cpp"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[
            (NodeKind::Class, "Box"),
            // `union Tag` — added in parity-r3 (cpp/queries.scm). The
            // Struct kind here is intentional (mirrors C parser; ref-
            // gitnexus emits Union and pairs via EQUIV).
            (NodeKind::Struct, "Tag"),
            (NodeKind::Typedef, "IntAlias"),
            (NodeKind::Namespace, "demo"),
        ],
        "Cpp",
    );
}

// -- Dart ---------------------------------------------------------------------

#[test]
fn dart_shape_pin() {
    use cgn_analyzer::dart::parser::DartProvider;
    let src = "\
class Box {\n  final int value;\n  const Box(this.value);\n  int add(int a, int b) => a + b;\n}\n\n\
double pi() => 3.14;\n";
    let p = DartProvider::new().expect("provider");
    let g = p.parse_file(Path::new("box.dart"), src.as_bytes()).expect("parse");
    assert_has(
        &g,
        &[(NodeKind::Class, "Box"), (NodeKind::Function, "pi")],
        "Dart",
    );
}
