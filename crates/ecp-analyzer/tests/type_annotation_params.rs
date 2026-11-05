//! A function that takes or returns a project type inside a generic, a
//! container, a union or an alias, or takes it as a parameter, reaches that
//! type with an `Accesses` edge of reason `type_annotation`, so `ecp impact`
//! on the type lists it. The single bare return type keeps its own path.
//!
//! Contracts shared by every language here:
//! - one edge per target per function, however many parameters name it;
//! - a name that is no project type (`Mystery`, a generic `T`) binds nothing;
//! - a qualified name binds the type of its qualifier, never a same-named
//!   type elsewhere, and a qualifier that resolves to nothing binds nothing
//!   (no retry on the last segment).
use crate::name_ref_support::{annotated, graph, refs};

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
use ecp_core::graph::{RelType, ZeroCopyGraph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// The `(file, name)` targets of `source`'s `type_annotation` edges, all in
/// `file`, must be exactly `names` (sorted by name).
fn assert_binds(g: &ZeroCopyGraph, file: &str, source: &str, names: &[&str]) {
    let want: Vec<_> = names.iter().map(|name| pair(file, name)).collect();
    assert_eq!(
        annotated(g, source),
        want,
        "type_annotation edges of {source}"
    );
}

fn build(provider: &dyn LanguageProvider, file: &str, src: &str) -> ZeroCopyGraph {
    graph(provider, &[(file, src)])
}

// ---------------------------------------------------------------- Rust

const RUST: &str = "\
pub struct St { pub n: u32 }
pub struct Other { pub n: u32 }
pub struct Raw;
pub type Al = Raw;
pub struct Owner;
pub fn nop() {}
pub fn bare() -> St { St { n: 0 } }
pub fn take(a: St) {}
pub fn make() -> Option<St> { None }
pub fn many(v: Vec<St>) {}
pub fn nested(m: std::collections::HashMap<String, Vec<St>>) {}
pub fn al_param(a: Al) {}
pub fn al_generic() -> Result<Option<Al>, String> { Ok(None) }
pub fn two(a: St, b: &St) {}
pub fn same(a: St) -> St { a }
pub fn both(a: St, b: Other) {}
pub fn ident<T>(a: T) -> T { a }
pub fn ext(a: Mystery) {}
pub fn ret_and_param(a: Other) -> Option<St> { None }
impl Owner {
    pub fn method(&self, a: St) {}
    pub fn option_self() -> Option<Self> { None }
}
";

#[test]
fn test_rust_param_and_generic_types_bind_each_project_type() {
    let g = build(&RustProvider::new().unwrap(), "src/app.rs", RUST);
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("many", &["St"]),
        ("nested", &["St"]),
        ("al_param", &["Al"]),
        ("al_generic", &["Al"]),
        ("two", &["St"]),
        ("same", &["St"]),
        ("both", &["Other", "St"]),
        ("ret_and_param", &["Other", "St"]),
        ("method", &["St"]),
    ] {
        assert_binds(&g, "src/app.rs", source, want);
    }
}

#[test]
fn test_rust_no_project_type_binds_nothing() {
    let g = build(&RustProvider::new().unwrap(), "src/app.rs", RUST);
    for source in ["nop", "ident", "ext", "option_self"] {
        assert_binds(&g, "src/app.rs", source, &[]);
    }
}

/// Contract: the bare return path is unchanged, and a parameter edge to the
/// same type carries the same confidence as the bare return edge.
#[test]
fn test_rust_bare_return_keeps_one_edge_and_confidence() {
    let g = build(&RustProvider::new().unwrap(), "src/app.rs", RUST);
    let bare = refs(&g, "bare", RelType::Accesses, "type_annotation");
    assert_eq!(bare.len(), 1, "{bare:?}");
    for source in ["same", "take"] {
        let edges = refs(&g, source, RelType::Accesses, "type_annotation");
        assert_eq!(edges.len(), 1, "{source}: {edges:?}");
        assert_eq!(edges[0].3, bare[0].3, "{source}: confidence");
    }
}

#[test]
fn test_rust_qualified_param_binds_the_qualifier_type_only() {
    let g = graph(
        &RustProvider::new().unwrap(),
        &[
            ("src/parse.rs", "pub struct Entry { pub n: u32 }\n"),
            ("src/other.rs", "pub struct Entry { pub m: u32 }\n"),
            ("src/shapes.rs", "pub struct Circle;\n"),
            (
                "src/lib.rs",
                "pub mod parse;\npub mod other;\npub mod shapes;\n\
                 pub fn q(a: parse::Entry) {}\n\
                 pub fn deep(a: Vec<crate::parse::Entry>) {}\n\
                 pub fn only(a: shapes::Circle) {}\n\
                 pub fn miss(a: nope::Entry) {}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "q"), [pair("src/parse.rs", "Entry")]);
    assert_eq!(annotated(&g, "deep"), [pair("src/parse.rs", "Entry")]);
    assert_eq!(annotated(&g, "only"), [pair("src/shapes.rs", "Circle")]);
    assert!(annotated(&g, "miss").is_empty());
}

// ---------------------------------------------------------- TypeScript

const TS: &str = "\
export class St { n = 1 }
export class Other { n = 2 }
export type Al = St;
export function nop(): void {}
export function bare(): St { return new St(); }
export function take(a: St): void {}
export function gen(): Promise<St> { return Promise.resolve(new St()); }
export function arr(v: Array<St>, w: St[]): void {}
export function nested(m: Map<string, Array<St>>): void {}
export function al(a: Al): void {}
export function alGen(): Promise<Al | null> { return Promise.resolve(null); }
export function two(a: St, b: St): void {}
export function uni(a: St | Other): void {}
export function nul(a: St | null): void {}
export function ident<T>(a: T): T { return a; }
export function ext(a: Mystery): void {}
export function untyped(a, b) { return a; }
";

#[test]
fn test_typescript_param_generic_and_alias_types_bind() {
    let g = build(&TypeScriptProvider::new().unwrap(), "app.ts", TS);
    for (source, want) in [
        ("take", &["St"][..]),
        ("gen", &["St"]),
        ("arr", &["St"]),
        ("nested", &["St"]),
        ("al", &["Al"]),
        ("alGen", &["Al"]),
        ("two", &["St"]),
        ("uni", &["Other", "St"]),
        ("nul", &["St"]),
    ] {
        assert_binds(&g, "app.ts", source, want);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

#[test]
fn test_typescript_no_project_type_binds_nothing() {
    let g = build(&TypeScriptProvider::new().unwrap(), "app.ts", TS);
    for source in ["nop", "ident", "ext", "untyped"] {
        assert_binds(&g, "app.ts", source, &[]);
    }
}

#[test]
fn test_typescript_qualified_param_binds_the_qualifier_type_only() {
    let g = graph(
        &TypeScriptProvider::new().unwrap(),
        &[
            ("types.ts", "export class St { n = 1 }\n"),
            ("other.ts", "export class St { m = 2 }\n"),
            (
                "app.ts",
                "import * as ns from './types';\n\
                 export function q(a: ns.St): void {}\n\
                 export function miss(a: nope.St): void {}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "q"), [pair("types.ts", "St")]);
    assert!(annotated(&g, "miss").is_empty());
}

// ---------------------------------------------------------- JavaScript

#[test]
fn test_javascript_untyped_params_bind_nothing() {
    let g = build(
        &JavaScriptProvider::new().unwrap(),
        "app.js",
        "export class St {}\n\
         /** @param {St} a */\n\
         export function take(a, b) { return a; }\n\
         export function nop() {}\n",
    );
    assert_binds(&g, "app.js", "take", &[]);
    assert_binds(&g, "app.js", "nop", &[]);
}

// -------------------------------------------------------------- Python

const PY: &str = "\
from typing import Optional, TypeAlias, TypeVar
T = TypeVar('T')
class St: pass
class Other: pass
Al: TypeAlias = St
def nop(): ...
def bare() -> St: ...
def take(a: St) -> None: ...
def make() -> Optional[St]: ...
def gen(v: list[St]) -> None: ...
def nested(m: dict[str, list[St]]) -> None: ...
def fwd(a: 'St') -> None: ...
def uni(a: St | Other) -> None: ...
def two(a: St, b: St) -> None: ...
def al(a: Al) -> None: ...
def al_gen(v: list[Al]) -> None: ...
def ident(a: T) -> T: ...
def ext(a: Mystery) -> None: ...
def untyped(a, b): ...
class Owner:
    def method(self, a: St) -> None: ...
";

#[test]
fn test_python_param_generic_union_and_alias_types_bind() {
    let g = build(&PythonProvider::new().unwrap(), "app.py", PY);
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("gen", &["St"]),
        ("nested", &["St"]),
        ("fwd", &["St"]),
        ("uni", &["Other", "St"]),
        ("two", &["St"]),
        ("al", &["Al"]),
        ("al_gen", &["Al"]),
        ("method", &["St"]),
    ] {
        assert_binds(&g, "app.py", source, want);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

#[test]
fn test_python_no_project_type_binds_nothing() {
    let g = build(&PythonProvider::new().unwrap(), "app.py", PY);
    for source in ["nop", "ident", "ext", "untyped"] {
        assert_binds(&g, "app.py", source, &[]);
    }
}

// ---------------------------------------------------------------- Java

const JAVA_APP: &str = "\
import java.util.*;
class App {
    void nop() {}
    St bare() { return null; }
    void take(St a) {}
    List<St> make() { return null; }
    void gen(List<St> v) {}
    void nested(Map<String, List<St>> m) {}
    void two(St a, St b) {}
    void both(St a, Other b) {}
    void arr(St[] a) {}
    <T> T ident(T a) { return a; }
    void ext(Mystery a) {}
}
";

#[test]
fn test_java_param_and_generic_types_bind() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("St.java", "class St { int n; }\n"),
            ("Other.java", "class Other { int n; }\n"),
            ("App.java", JAVA_APP),
        ],
    );
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("gen", &["St"]),
        ("nested", &["St"]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
        ("arr", &["St"]),
    ] {
        let want: Vec<_> = want.iter().map(|n| pair(&format!("{n}.java"), n)).collect();
        assert_eq!(annotated(&g, source), want, "{source}");
    }
    for source in ["nop", "ident", "ext"] {
        assert!(annotated(&g, source).is_empty(), "{source}");
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// -------------------------------------------------------------- Kotlin

const KOTLIN: &str = "\
class St
class Other
typealias Al = St
fun nop() {}
fun bare(): St = St()
fun take(a: St) {}
fun make(): List<St> = listOf()
fun gen(v: List<St>?) {}
fun nested(m: Map<String, List<St>>) {}
fun al(a: Al) {}
fun alGen(): List<Al> = listOf()
fun two(a: St, b: St) {}
fun both(a: St, b: Other) {}
fun <T> ident(a: T): T = a
fun ext(a: Mystery) {}
";

#[test]
fn test_kotlin_param_generic_and_alias_types_bind() {
    let g = build(&KotlinProvider::new().unwrap(), "app.kt", KOTLIN);
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("gen", &["St"]),
        ("nested", &["St"]),
        ("al", &["Al"]),
        ("alGen", &["Al"]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
    ] {
        assert_binds(&g, "app.kt", source, want);
    }
    for source in ["nop", "ident", "ext"] {
        assert_binds(&g, "app.kt", source, &[]);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// ------------------------------------------------------------------ C#

const CSHARP: &str = "\
using System.Collections.Generic;
class St { }
class Other { }
class App {
    void Nop() { }
    St Bare() { return null; }
    void Take(St a) { }
    List<St> Make() { return null; }
    void Gen(List<St> v) { }
    void Nested(Dictionary<string, List<St>> m) { }
    void Two(St a, St b) { }
    void Both(St a, Other b) { }
    void Arr(St[] a) { }
    void Nul(St? a) { }
    T Ident<T>(T a) { return a; }
    void Ext(Mystery a) { }
}
";

#[test]
fn test_csharp_param_and_generic_types_bind() {
    let g = build(&CSharpProvider::new().unwrap(), "App.cs", CSHARP);
    for (source, want) in [
        ("Take", &["St"][..]),
        ("Make", &["St"]),
        ("Gen", &["St"]),
        ("Nested", &["St"]),
        ("Two", &["St"]),
        ("Both", &["Other", "St"]),
        ("Arr", &["St"]),
        ("Nul", &["St"]),
    ] {
        assert_binds(&g, "App.cs", source, want);
    }
    for source in ["Nop", "Ident", "Ext"] {
        assert_binds(&g, "App.cs", source, &[]);
    }
    assert_eq!(
        refs(&g, "Bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// ------------------------------------------------------------------ Go

const GO: &str = "\
package main

type St struct{ N int }
type Other struct{}
type Al = St

func Nop() {}
func Bare() St { return St{} }
func Take(a St) {}
func Make() []St { return nil }
func Gen(v []St, m map[string]*St) {}
func Alias(a Al) {}
func AliasGen() []Al { return nil }
func Two(a St, b *St) {}
func Both(a St, b Other) {}
func Ident[T any](a T) T { return a }
func Ext(a Mystery) {}
";

#[test]
fn test_go_param_slice_map_and_alias_types_bind() {
    let g = build(&GoProvider::new().unwrap(), "app.go", GO);
    for (source, want) in [
        ("Take", &["St"][..]),
        ("Make", &["St"]),
        ("Gen", &["St"]),
        ("Alias", &["Al"]),
        ("AliasGen", &["Al"]),
        ("Two", &["St"]),
        ("Both", &["Other", "St"]),
    ] {
        assert_binds(&g, "app.go", source, want);
    }
    for source in ["Nop", "Ident", "Ext"] {
        assert_binds(&g, "app.go", source, &[]);
    }
    assert_eq!(
        refs(&g, "Bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

#[test]
fn test_go_qualified_param_binds_the_qualifier_package_only() {
    let g = graph(
        &GoProvider::new().unwrap(),
        &[
            ("types/t.go", "package types\n\ntype St struct{ N int }\n"),
            ("other/o.go", "package other\n\ntype St struct{ M int }\n"),
            (
                "main.go",
                "package main\n\nimport \"app/types\"\n\n\
                 func Q(a []types.St) {}\n\
                 func Miss(a []nope.St) {}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "Q"), [pair("types/t.go", "St")]);
    assert!(annotated(&g, "Miss").is_empty());
}

// ----------------------------------------------------------------- PHP

const PHP: &str = "\
<?php
class St {}
class Other {}
function nop() {}
function bare(): St { return new St(); }
function take(St $a) {}
function nullable(?St $a): ?St { return $a; }
function uni(St|Other $a) {}
function two(St $a, St $b) {}
function ext(Mystery $a) {}
function arr(array $a) {}
";

#[test]
fn test_php_param_nullable_and_union_types_bind() {
    let g = build(&PhpProvider::new().unwrap(), "app.php", PHP);
    for (source, want) in [
        ("take", &["St"][..]),
        ("nullable", &["St"]),
        ("uni", &["Other", "St"]),
        ("two", &["St"]),
    ] {
        assert_binds(&g, "app.php", source, want);
    }
    for source in ["nop", "ext", "arr"] {
        assert_binds(&g, "app.php", source, &[]);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// ---------------------------------------------------------------- Ruby

#[test]
fn test_ruby_untyped_params_bind_nothing() {
    let g = build(
        &RubyProvider::new().unwrap(),
        "app.rb",
        "class St; end\ndef take(a, b = 1)\n  a\nend\ndef nop\nend\n",
    );
    assert_binds(&g, "app.rb", "take", &[]);
    assert_binds(&g, "app.rb", "nop", &[]);
}

// --------------------------------------------------------------- Swift

const SWIFT: &str = "\
struct St {}
struct Other {}
typealias Al = St
func nop() {}
func take(a: St) {}
func gen(v: [St]) {}
func opt(a: Optional<St>, b: St?) {}
func dict(m: [String: St]) {}
func al(a: Al) {}
func alGen(v: [Al]) {}
func two(a: St, b: St) {}
func both(a: St, b: Other) {}
func ident<T>(a: T) {}
func ext(a: Mystery) {}
";

/// The Swift parser keeps no function return annotation, so only parameter
/// types are covered here.
#[test]
fn test_swift_param_generic_and_alias_types_bind() {
    let g = build(&SwiftProvider::new().unwrap(), "app.swift", SWIFT);
    for (source, want) in [
        ("take", &["St"][..]),
        ("gen", &["St"]),
        ("opt", &["St"]),
        ("dict", &["St"]),
        ("al", &["Al"]),
        ("alGen", &["Al"]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
    ] {
        assert_binds(&g, "app.swift", source, want);
    }
    for source in ["nop", "ident", "ext"] {
        assert_binds(&g, "app.swift", source, &[]);
    }
}

// ------------------------------------------------------------------- C

const C_SRC: &str = "\
struct St { int n; };
struct Other { int n; };
typedef struct St Al;
void nop(void) {}
struct St *bare(void) { return 0; }
void take(struct St *a) {}
void two(struct St a, const struct St *b) {}
void both(struct St *a, struct Other *b) {}
void alias(Al *a) {}
void ext(struct Mystery *a) {}
";

#[test]
fn test_c_param_pointer_and_typedef_types_bind() {
    let g = build(&CProvider::new().unwrap(), "app.c", C_SRC);
    for (source, want) in [
        ("take", &["St"][..]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
        ("alias", &["Al"]),
    ] {
        assert_binds(&g, "app.c", source, want);
    }
    for source in ["nop", "ext"] {
        assert_binds(&g, "app.c", source, &[]);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// ----------------------------------------------------------------- C++

const CPP: &str = "\
#include <vector>
#include <map>
struct St { int n; };
struct Other { int n; };
using Al = St;
void nop() {}
St bare() { return St{}; }
void take(St a) {}
std::vector<St> make() { return {}; }
void gen(std::vector<St> v) {}
void nested(std::map<std::string, std::vector<St>> m) {}
void ref(const St& a) {}
void al(Al a) {}
void alGen(std::vector<Al> v) {}
void two(St a, St* b) {}
void both(St a, Other b) {}
template <typename T> T ident(T a) { return a; }
void ext(Mystery a) {}
";

#[test]
fn test_cpp_param_template_and_alias_types_bind() {
    let g = build(&CppProvider::new().unwrap(), "app.cpp", CPP);
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("gen", &["St"]),
        ("nested", &["St"]),
        ("ref", &["St"]),
        ("al", &["Al"]),
        ("alGen", &["Al"]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
    ] {
        assert_binds(&g, "app.cpp", source, want);
    }
    for source in ["nop", "ident", "ext"] {
        assert_binds(&g, "app.cpp", source, &[]);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

#[test]
fn test_cpp_qualified_param_binds_the_namespace_type_only() {
    let g = graph(
        &CppProvider::new().unwrap(),
        &[
            ("a.h", "namespace a { struct St { int n; }; }\n"),
            ("b.h", "namespace b { struct St { int m; }; }\n"),
            (
                "m.cpp",
                "#include \"a.h\"\n#include \"b.h\"\n#include <vector>\n\
                 void q(a::St x) {}\n\
                 void p(std::vector<b::St> v) {}\n\
                 void miss(z::St x) {}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "q"), [pair("a.h", "St")]);
    assert_eq!(annotated(&g, "p"), [pair("b.h", "St")]);
    assert!(annotated(&g, "miss").is_empty());
}

// ---------------------------------------------------------------- Dart

const DART: &str = "\
class St {}
class Other {}
typedef Al = St;
void nop() {}
St bare() => St();
void take(St a) {}
Future<St> make() async => St();
void gen(List<St> v) {}
void nested(Map<String, List<St>> m) {}
void al(Al a) {}
List<Al> alGen() => [];
void two(St a, St? b) {}
void both(St a, Other b) {}
T ident<T>(T a) => a;
void ext(Mystery a) {}
";

#[test]
fn test_dart_param_generic_and_alias_types_bind() {
    let g = build(&DartProvider::new().unwrap(), "app.dart", DART);
    for (source, want) in [
        ("take", &["St"][..]),
        ("make", &["St"]),
        ("gen", &["St"]),
        ("nested", &["St"]),
        ("al", &["Al"]),
        ("alGen", &["Al"]),
        ("two", &["St"]),
        ("both", &["Other", "St"]),
    ] {
        assert_binds(&g, "app.dart", source, want);
    }
    for source in ["nop", "ident", "ext"] {
        assert_binds(&g, "app.dart", source, &[]);
    }
    assert_eq!(
        refs(&g, "bare", RelType::Accesses, "type_annotation").len(),
        1
    );
}

// ------------------------- Parameter names inside a function type (F4 item 4)
// A function type names its parameters (`func(a St)`, `(a: St) => void`),
// and the name is never a type: with a project type named `a`, only `St`
// binds. An unnamed parameter (`func(a, St)`, `(a) -> St`) is a type.
// Rust, Java, C#, PHP and Ruby function types carry no parameter names;
// JavaScript has no annotations. C is covered by the scanner unit test
// (`test_annotation_type_names_function_type_parameter_names_excluded`).

const GO_FN_TYPES: &str = "\
package app
type a struct{}
type St struct{}
type Out struct{}
func Reg(cb func(a St) (Out, error)) {}
func RegShared(cb func(a, b St)) {}
func RegUnnamed(cb func(a, St)) {}
";

#[test]
fn test_go_function_type_parameter_names_bind_nothing() {
    let g = build(&GoProvider::new().unwrap(), "app.go", GO_FN_TYPES);
    assert_binds(&g, "app.go", "Reg", &["Out", "St"]);
    assert_binds(&g, "app.go", "RegShared", &["St"]);
    assert_binds(&g, "app.go", "RegUnnamed", &["St", "a"]);
}

const TS_FN_TYPES: &str = "\
export class a { n = 1 }
export class St { n = 2 }
export function reg(cb: (a: St) => void): void {}
export function regOpt(cb: (a?: St) => void): void {}
export function regObj(o: { a: St }): void {}
export function regUnnamed(cb: (x: a) => St): void {}
";

#[test]
fn test_typescript_function_type_parameter_names_bind_nothing() {
    let g = build(&TypeScriptProvider::new().unwrap(), "app.ts", TS_FN_TYPES);
    for source in ["reg", "regOpt", "regObj"] {
        assert_binds(&g, "app.ts", source, &["St"]);
    }
    assert_binds(&g, "app.ts", "regUnnamed", &["St", "a"]);
}

const KOTLIN_FN_TYPES: &str = "\
class a
class St
fun reg(cb: (a: St) -> Unit) {}
fun regUnnamed(cb: (a) -> St) {}
";

#[test]
fn test_kotlin_function_type_parameter_names_bind_nothing() {
    let g = build(&KotlinProvider::new().unwrap(), "app.kt", KOTLIN_FN_TYPES);
    assert_binds(&g, "app.kt", "reg", &["St"]);
    assert_binds(&g, "app.kt", "regUnnamed", &["St", "a"]);
}

const SWIFT_FN_TYPES: &str = "\
struct a {}
struct St {}
func reg(_ cb: (_ a: St) -> Void) {}
func regUnnamed(_ cb: (a) -> St) {}
";

#[test]
fn test_swift_function_type_parameter_names_bind_nothing() {
    let g = build(&SwiftProvider::new().unwrap(), "app.swift", SWIFT_FN_TYPES);
    assert_binds(&g, "app.swift", "reg", &["St"]);
    assert_binds(&g, "app.swift", "regUnnamed", &["St", "a"]);
}

const DART_FN_TYPES: &str = "\
class a {}
class St {}
void reg(void Function(St a) cb) {}
void regUnnamed(void Function(a) cb) {}
";

#[test]
fn test_dart_function_type_parameter_names_bind_nothing() {
    let g = build(&DartProvider::new().unwrap(), "app.dart", DART_FN_TYPES);
    assert_binds(&g, "app.dart", "reg", &["St"]);
    assert_binds(&g, "app.dart", "regUnnamed", &["a"]);
}

const CPP_FN_TYPES: &str = "\
#include <functional>
struct a {};
struct St {};
void reg(std::function<void(St a)> cb) {}
void regUnnamed(std::function<void(a)> cb) {}
";

#[test]
fn test_cpp_function_type_parameter_names_bind_nothing() {
    let g = build(&CppProvider::new().unwrap(), "app.cpp", CPP_FN_TYPES);
    assert_binds(&g, "app.cpp", "reg", &["St"]);
    assert_binds(&g, "app.cpp", "regUnnamed", &["a"]);
}

// ------------------------- Python value-only subscripts (F4 item 5)
// `Literal["St"]` holds values and `Annotated[T, "St"]` metadata after its
// first argument: neither string is a forward reference. Only Python spells
// a type as a string inside a subscript.

const PY_LITERALS: &str = "\
from typing import Annotated, Callable, Literal, Optional
class St: pass
class Other: pass
class Ünï: pass
def lit(mode: Literal['St', 'Other']) -> None: ...
def lit_nested(m: Optional[Literal['St']]) -> None: ...
def lit_unicode(m: Literal['Ünï']) -> None: ...
def meta(x: Annotated[int, 'St']) -> None: ...
def meta_type(x: Annotated[St, 'Other']) -> None: ...
def meta_fwd(x: Annotated['St', 'Other']) -> None: ...
def fwd_unicode(x: 'Ünï') -> None: ...
def cb(f: Callable[[St], Other]) -> None: ...
";

#[test]
fn test_python_literal_and_annotated_metadata_bind_nothing() {
    let g = build(&PythonProvider::new().unwrap(), "app.py", PY_LITERALS);
    for source in ["lit", "lit_nested", "lit_unicode", "meta"] {
        assert_binds(&g, "app.py", source, &[]);
    }
    assert_binds(&g, "app.py", "meta_type", &["St"]);
    assert_binds(&g, "app.py", "meta_fwd", &["St"]);
    assert_binds(&g, "app.py", "fwd_unicode", &["Ünï"]);
    assert_binds(&g, "app.py", "cb", &["Other", "St"]);
}
