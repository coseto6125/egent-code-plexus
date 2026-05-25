//! 14-language coverage for the `ReadsField` edge (LLM-utility (A) graph
//! completeness — `ecp impact <field>` must reach a field's readers).
//!
//! Each case parses a single source that defines a type with a field and a
//! free function / method that reads it, then asserts a `ReadsField` edge to
//! the field's `Property` node exists. The parser captures the member-access
//! read, the resolver wires it to the Property target, the builder emits.
//!
//! Two of the 14 languages are pinned as negative cases rather than positive,
//! each for a concrete reason (kept explicit so neither is a silent gap):
//!   - Ruby: `obj.attr` is syntactically a method call (no distinct
//!     member-access node), already covered by `Calls`; a ReadsField capture
//!     would be a false positive. See `ruby_attr_read_is_call_not_field`.
//!   - JavaScript: class fields are not modeled as `Property` nodes (no
//!     property capture in its queries.scm, unlike TS), so the read has no
//!     resolvable target. See `javascript_field_read_has_no_property_target`.

use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build(provider: &dyn LanguageProvider, path: &str, src: &str) -> ZeroCopyGraph {
    let local = provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file");
    let mut builder = ecp_analyzer::resolution::builder::GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

/// Assert at least one ReadsField edge targets a Property whose name is `field`.
fn assert_reads_field(g: &ZeroCopyGraph, field: &str) {
    let hit = g.edges.iter().any(|e| {
        e.rel_type == RelType::ReadsField && {
            let t = &g.nodes[e.target as usize];
            t.kind == NodeKind::Property && t.name.resolve(&g.string_pool) == field
        }
    });
    assert!(
        hit,
        "expected a ReadsField edge to Property `{field}`.\nReadsField edges: {:?}\nProperties: {:?}",
        g.edges
            .iter()
            .filter(|e| e.rel_type == RelType::ReadsField)
            .map(|e| (
                g.nodes[e.source as usize].name.resolve(&g.string_pool),
                g.nodes[e.target as usize].name.resolve(&g.string_pool)
            ))
            .collect::<Vec<_>>(),
        g.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Property)
            .map(|n| n.name.resolve(&g.string_pool))
            .collect::<Vec<_>>()
    );
}

#[test]
fn typescript_reads_field() {
    let p = ecp_analyzer::typescript::parser::TypeScriptProvider::new().unwrap();
    let src = r#"
class Config { timeout: number = 0; }
function readTimeout(c: Config): number { return c.timeout; }
"#;
    assert_reads_field(&build(&p, "a.ts", src), "timeout");
}

/// JavaScript does not model class fields as `Property` nodes (its
/// `queries.scm` has no property capture — unlike TypeScript's
/// `public_field_definition`), so a `c.timeout` read has no Property target to
/// resolve against and `ReadsField` does not fire. The member-access capture
/// itself works (verified by the TS sibling); this is a JS node-coverage gap
/// tracked as a follow-up, pinned here so it is explicit, not silent.
#[test]
fn javascript_field_read_has_no_property_target() {
    let p = ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap();
    let src = r#"
class Config { timeout = 0; }
function readTimeout(c) { return c.timeout; }
"#;
    let g = build(&p, "a.js", src);
    let has_reads_field = g.edges.iter().any(|e| e.rel_type == RelType::ReadsField);
    assert!(
        !has_reads_field,
        "JS class fields are not Property nodes yet; ReadsField cannot resolve"
    );
}

#[test]
fn python_reads_field() {
    let p = ecp_analyzer::python::parser::PythonProvider::new().unwrap();
    let src = r#"
class Config:
    timeout: int = 0

def read_timeout(c):
    return c.timeout
"#;
    assert_reads_field(&build(&p, "a.py", src), "timeout");
}

#[test]
fn java_reads_field() {
    let p = ecp_analyzer::java::parser::JavaProvider::new().unwrap();
    let src = r#"
class Config { public int timeout; }
class Reader { int read(Config c) { return c.timeout; } }
"#;
    assert_reads_field(&build(&p, "A.java", src), "timeout");
}

#[test]
fn kotlin_reads_field() {
    let p = ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap();
    let src = r#"
class Config { val timeout: Int = 0 }
fun readTimeout(c: Config): Int { return c.timeout }
"#;
    assert_reads_field(&build(&p, "a.kt", src), "timeout");
}

#[test]
fn csharp_reads_field() {
    let p = ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap();
    let src = r#"
class Config { public int Timeout; }
class Reader { int Read(Config c) { return c.Timeout; } }
"#;
    assert_reads_field(&build(&p, "A.cs", src), "Timeout");
}

#[test]
fn go_reads_field() {
    let p = ecp_analyzer::go::parser::GoProvider::new().unwrap();
    let src = r#"
package main
type Config struct { Timeout int }
func readTimeout(c Config) int { return c.Timeout }
"#;
    assert_reads_field(&build(&p, "a.go", src), "Timeout");
}

#[test]
fn rust_reads_field() {
    let p = ecp_analyzer::rust::parser::RustProvider::new().unwrap();
    let src = r#"
pub struct Config { pub timeout: u32 }
fn read_timeout(c: &Config) -> u32 { c.timeout }
"#;
    assert_reads_field(&build(&p, "a.rs", src), "timeout");
}

#[test]
fn php_reads_field() {
    let p = ecp_analyzer::php::parser::PhpProvider::new().unwrap();
    let src = r#"<?php
class Config { public int $timeout = 0; }
function readTimeout(Config $c): int { return $c->timeout; }
"#;
    assert_reads_field(&build(&p, "a.php", src), "timeout");
}

#[test]
fn swift_reads_field() {
    let p = ecp_analyzer::swift::parser::SwiftProvider::new().unwrap();
    let src = r#"
class Config { var timeout: Int = 0 }
func readTimeout(c: Config) -> Int { return c.timeout }
"#;
    assert_reads_field(&build(&p, "a.swift", src), "timeout");
}

#[test]
fn c_reads_field() {
    let p = ecp_analyzer::c::parser::CProvider::new().unwrap();
    let src = r#"
struct Config { int timeout; };
int read_timeout(struct Config *c) { return c->timeout; }
"#;
    assert_reads_field(&build(&p, "a.c", src), "timeout");
}

#[test]
fn cpp_reads_field() {
    let p = ecp_analyzer::cpp::parser::CppProvider::new().unwrap();
    let src = r#"
struct Config { int timeout; };
int read_timeout(Config* c) { return c->timeout; }
"#;
    assert_reads_field(&build(&p, "a.cpp", src), "timeout");
}

#[test]
fn dart_reads_field() {
    let p = ecp_analyzer::dart::parser::DartProvider::new().unwrap();
    let src = r#"
class Config { int timeout = 0; }
int readTimeout(Config c) { return c.timeout; }
"#;
    assert_reads_field(&build(&p, "a.dart", src), "timeout");
}

/// Ruby's `c.timeout` is a `call` node, not a distinct member-access — it is
/// recorded as a `Calls` edge, so `ReadsField` deliberately does not fire.
/// Pinning this keeps the 14th language's behavior explicit rather than a
/// silent gap.
#[test]
fn ruby_attr_read_is_call_not_field() {
    let p = ecp_analyzer::ruby::parser::RubyProvider::new().unwrap();
    let src = r#"
class Config
  attr_reader :timeout
end
def read_timeout(c)
  c.timeout
end
"#;
    let g = build(&p, "a.rb", src);
    let has_reads_field = g.edges.iter().any(|e| e.rel_type == RelType::ReadsField);
    assert!(
        !has_reads_field,
        "Ruby attr access is a method call (Calls edge), not ReadsField"
    );
}
