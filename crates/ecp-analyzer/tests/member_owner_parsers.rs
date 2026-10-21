//! The owner each parser gives a member, which the resolver's same-file pick
//! reads as the type that declares it (uid = kind + path + owner + name).
//!
//! - Rust `impl` self types: a generic (`Pin<P>`), path-qualified
//!   (`io::Cursor<u16>`) or reference (`&'a mut Dog`) self type owns its
//!   methods by its last path segment, without generic arguments. A
//!   path-qualified self type emits an `Impl` node as a plain one does. A
//!   blanket `impl<T> W for T` keeps the type parameter `T` as owner: no
//!   type of that name exists, so the member reads as owned, not free.
//! - Rust trait heritage: `impl fmt::Display`, `impl From<u8>`,
//!   `impl io::Write` record the trait path without generic arguments
//!   (`fmt::Display`, `From`, `io::Write`), as C++ records `detail::foo`.
//! - Enum members: a method, constructor or field in an enum body is owned
//!   by the enum (`stamp_owner_class_by_span`), and a type nested in an enum
//!   too. A Kotlin `fun` in an enum body is a `Method`, as in a class body.
//!   Each of the 14 mainstream languages has a guard here, with an enum
//!   member where the language has one and its nearest analogue otherwise.
//! - C++ `void ns::f() {}` with `ns` a namespace of the file is a free
//!   `Function` owned by `ns`, as a `void f() {}` inside `namespace ns`.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn parse(provider: &dyn LanguageProvider, path: &str, source: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), source.as_bytes())
        .unwrap()
}

fn build(provider: &dyn LanguageProvider, path: &str, source: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(provider, path, source));
    builder.build()
}

fn summary(lg: &LocalGraph) -> Vec<(String, NodeKind, Option<String>, u32)> {
    lg.nodes
        .iter()
        .map(|n| (n.name.clone(), n.kind, n.owner_class.clone(), n.span.0))
        .collect()
}

/// The one node named `name` that starts on 0-based `row`, other than a
/// `File` node.
fn node_at<'a>(lg: &'a LocalGraph, name: &str, row: u32) -> &'a RawNode {
    let hits: Vec<&RawNode> = lg
        .nodes
        .iter()
        .filter(|n| n.name == name && n.span.0 == row && n.kind != NodeKind::File)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "one `{name}` on row {row}; nodes: {:?}",
        summary(lg)
    );
    hits[0]
}

/// Every node named `name` of `kind` exists and is owned by `owner`.
fn assert_owner(lg: &LocalGraph, name: &str, kind: NodeKind, owner: &str) {
    let hits: Vec<&RawNode> = lg
        .nodes
        .iter()
        .filter(|n| n.name == name && n.kind == kind)
        .collect();
    assert!(
        !hits.is_empty(),
        "{}: no {kind:?} `{name}`; nodes: {:?}",
        lg.file_path.display(),
        summary(lg)
    );
    for n in hits {
        assert_eq!(
            n.owner_class.as_deref(),
            Some(owner),
            "{}: {kind:?} `{name}` on row {} is owned by `{owner}`",
            lg.file_path.display(),
            n.span.0
        );
    }
}

/// The node named `name` on `row` has `kind` and `owner` (`None`: no owner).
fn assert_at(lg: &LocalGraph, name: &str, row: u32, kind: NodeKind, owner: Option<&str>) {
    let n = node_at(lg, name, row);
    assert_eq!(
        (n.kind, n.owner_class.as_deref()),
        (kind, owner),
        "`{name}` on row {row}"
    );
}

fn node_name(graph: &ZeroCopyGraph, id: u32) -> &str {
    graph.nodes[id as usize].name.resolve(&graph.string_pool)
}

fn node_owner(graph: &ZeroCopyGraph, id: u32) -> &str {
    graph.nodes[id as usize]
        .owner_class
        .resolve(&graph.string_pool)
}

fn typescript() -> ecp_analyzer::typescript::TypeScriptProvider {
    ecp_analyzer::typescript::TypeScriptProvider::new().unwrap()
}
fn javascript() -> ecp_analyzer::javascript::parser::JavaScriptProvider {
    ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap()
}
fn python() -> ecp_analyzer::python::PythonProvider {
    ecp_analyzer::python::PythonProvider::new().unwrap()
}
fn java() -> ecp_analyzer::java::parser::JavaProvider {
    ecp_analyzer::java::parser::JavaProvider::new().unwrap()
}
fn kotlin() -> ecp_analyzer::kotlin::parser::KotlinProvider {
    ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap()
}
fn csharp() -> ecp_analyzer::c_sharp::parser::CSharpProvider {
    ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap()
}
fn go() -> ecp_analyzer::go::parser::GoProvider {
    ecp_analyzer::go::parser::GoProvider::new().unwrap()
}
fn rust() -> ecp_analyzer::rust::parser::RustProvider {
    ecp_analyzer::rust::parser::RustProvider::new().unwrap()
}
fn php() -> ecp_analyzer::php::parser::PhpProvider {
    ecp_analyzer::php::parser::PhpProvider::new().unwrap()
}
fn ruby() -> ecp_analyzer::ruby::parser::RubyProvider {
    ecp_analyzer::ruby::parser::RubyProvider::new().unwrap()
}
fn swift() -> ecp_analyzer::swift::parser::SwiftProvider {
    ecp_analyzer::swift::parser::SwiftProvider::new().unwrap()
}
fn c() -> ecp_analyzer::c::parser::CProvider {
    ecp_analyzer::c::parser::CProvider::new().unwrap()
}
fn cpp() -> ecp_analyzer::cpp::parser::CppProvider {
    ecp_analyzer::cpp::parser::CppProvider::new().unwrap()
}
fn dart() -> ecp_analyzer::dart::parser::DartProvider {
    ecp_analyzer::dart::parser::DartProvider::new().unwrap()
}

// ---- Rust impl self types ----

/// One `poll_write` per row; tokio's `AsyncWrite` impls in shape.
const RUST_SELF_TYPES: &str = "pub trait W { fn poll_write(&self) -> u8; }\n\
impl<P> W for Pin<P> where P: Clone { fn poll_write(&self) -> u8 { 1 } }\n\
impl W for Cursor<&mut [u8]> { fn poll_write(&self) -> u8 { 2 } }\n\
impl W for Vec<u8> { fn poll_write(&self) -> u8 { 3 } }\n\
impl W for io::Cursor<u16> { fn poll_write(&self) -> u8 { 4 } }\n\
impl W for io::Stdout { fn poll_write(&self) -> u8 { 5 } }\n\
impl<'a> W for &'a mut Dog { fn poll_write(&self) -> u8 { 6 } }\n\
impl<T: Clone> W for T { fn poll_write(&self) -> u8 { 7 } }\n";

#[test]
fn test_parse_file_rust_generic_and_scoped_impl_self_type_owns_methods_by_last_segment() {
    let lg = parse(&rust(), "src/aw.rs", RUST_SELF_TYPES);
    let expected = [
        (0, "W"),
        (1, "Pin"),
        (2, "Cursor"),
        (3, "Vec"),
        (4, "Cursor"),
        (5, "Stdout"),
        (6, "Dog"),
    ];
    for (row, owner) in expected {
        assert_at(&lg, "poll_write", row, NodeKind::Method, Some(owner));
    }
}

#[test]
fn test_parse_file_rust_blanket_impl_keeps_type_parameter_owner() {
    let lg = parse(&rust(), "src/aw.rs", RUST_SELF_TYPES);
    assert_at(&lg, "poll_write", 7, NodeKind::Method, Some("T"));
}

#[test]
fn test_parse_file_rust_scoped_impl_self_type_emits_impl_node() {
    let lg = parse(&rust(), "src/aw.rs", RUST_SELF_TYPES);
    assert_at(&lg, "Cursor", 4, NodeKind::Impl, Some("W"));
    assert_at(&lg, "Stdout", 5, NodeKind::Impl, Some("W"));
    assert_at(&lg, "Pin", 1, NodeKind::Impl, Some("W"));
}

/// `Cursor<&mut [u8]>` and `io::Cursor<u16>` share owner `Cursor`: the
/// second `poll_write` is an overload of the first, kept as its own node.
#[test]
fn test_build_rust_two_impls_of_one_generic_type_keeps_both_methods() {
    let graph = build(&rust(), "src/aw.rs", RUST_SELF_TYPES);
    let cursor_rows: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| node_name(&graph, i) == "poll_write" && node_owner(&graph, i) == "Cursor")
        .map(|i| graph.nodes[i as usize].span.0)
        .collect();
    assert_eq!(cursor_rows.len(), 2, "two `Cursor` blocks: {cursor_rows:?}");
}

/// `self.b()` inside `impl W for Pin<P>` types `self` as `Pin`.
#[test]
fn test_resolve_call_rust_self_call_in_generic_impl_binds_owner_member() {
    let graph = build(
        &rust(),
        "src/lib.rs",
        "pub trait W { fn a(&self) -> u8; fn b(&self) -> u8; }\n\
         impl<P> W for Pin<P> { fn a(&self) -> u8 { self.b() } fn b(&self) -> u8 { 0 } }\n\
         impl W for Vec<u8> { fn a(&self) -> u8 { 1 } fn b(&self) -> u8 { 1 } }\n",
    );
    let caller = (0..graph.nodes.len() as u32)
        .find(|&i| node_name(&graph, i) == "a" && node_owner(&graph, i) == "Pin")
        .expect("`Pin`'s `a`");
    let targets: Vec<(String, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == caller)
        .filter(|e| node_name(&graph, e.target) == "b")
        .map(|e| {
            (
                node_owner(&graph, e.target).to_string(),
                graph.nodes[e.target as usize].span.0,
            )
        })
        .collect();
    assert_eq!(targets, vec![("Pin".to_string(), 1)]);
}

// ---- Rust trait heritage ----

#[test]
fn test_parse_file_rust_scoped_and_generic_trait_impl_records_trait_path() {
    let lg = parse(
        &rust(),
        "src/lib.rs",
        "pub struct X;\n\
         impl fmt::Display for X { fn fmt(&self) -> u8 { 0 } }\n\
         impl From<u8> for X { fn from(v: u8) -> X { X } }\n\
         impl io::Write for X { fn flush(&mut self) -> u8 { 0 } }\n\
         impl Iterator<Item = u8> for X { fn next(&mut self) -> Option<u8> { None } }\n\
         impl Plain for X { fn plain(&self) {} }\n",
    );
    let expected = [
        ("fmt", 1, "fmt::Display"),
        ("from", 2, "From"),
        ("flush", 3, "io::Write"),
        ("next", 4, "Iterator"),
        ("plain", 5, "Plain"),
    ];
    for (name, row, base) in expected {
        let n = node_at(&lg, name, row);
        assert_eq!(n.heritage, vec![base.to_string()], "`{name}` heritage");
        assert_eq!(n.owner_class.as_deref(), Some("X"), "`{name}` owner");
    }
}

/// The trait of `impl m::T for X` and of `impl G<u8> for X` is the file's
/// own trait, so each method implements it.
#[test]
fn test_build_rust_scoped_and_generic_trait_impl_emits_implements_edge() {
    let graph = build(
        &rust(),
        "src/lib.rs",
        "pub mod m { pub trait T { fn t(&self); } }\n\
         pub trait G<A> { fn g(&self); }\n\
         pub struct X;\n\
         impl m::T for X { fn t(&self) {} }\n\
         impl G<u8> for X { fn g(&self) {} }\n",
    );
    for (member, row, base) in [("t", 3, "T"), ("g", 4, "G")] {
        let implements = graph.edges.iter().any(|e| {
            e.rel_type == RelType::Implements
                && node_name(&graph, e.source) == member
                && graph.nodes[e.source as usize].span.0 == row
                && node_name(&graph, e.target) == base
                && graph.nodes[e.target as usize].kind == NodeKind::Trait
        });
        assert!(implements, "`{member}` implements `{base}`");
    }
}

// ---- Enum members, 14 languages ----

#[test]
fn test_parse_file_java_enum_members_owned_by_enum() {
    let lg = parse(
        &java(),
        "Color.java",
        "package p;\n\
         enum Color {\n\
           RED, GREEN;\n\
           int code;\n\
           Color() {}\n\
           void describe() {}\n\
         }\n\
         class Outer {\n\
           enum In {\n\
             A;\n\
             void m() {}\n\
           }\n\
         }\n",
    );
    assert_owner(&lg, "describe", NodeKind::Method, "Color");
    assert_owner(&lg, "Color", NodeKind::Constructor, "Color");
    assert_owner(&lg, "code", NodeKind::Property, "Color");
    assert_owner(&lg, "RED", NodeKind::EnumVariant, "Color");
    assert_owner(&lg, "m", NodeKind::Method, "In");
    assert_owner(&lg, "In", NodeKind::Enum, "Outer");
}

const KOTLIN_ENUM: &str = "package p\n\
enum class Color {\n\
    RED {\n\
        override fun describe(): String = \"r\"\n\
    },\n\
    GREEN;\n\
    open fun describe(): String = \"c\"\n\
    val code: Int get() = 1\n\
    companion object {\n\
        fun parse(s: String): Color = RED\n\
    }\n\
}\n\
class Outer {\n\
    enum class In {\n\
        A;\n\
        fun m(): Int = 1\n\
    }\n\
}\n\
class Box { fun describe(): String = \"b\" }\n";

#[test]
fn test_parse_file_kotlin_enum_fun_is_method_owned_by_enum() {
    let lg = parse(&kotlin(), "Color.kt", KOTLIN_ENUM);
    assert_at(&lg, "describe", 6, NodeKind::Method, Some("Color"));
    assert_at(&lg, "describe", 18, NodeKind::Method, Some("Box"));
}

/// An entry body overrides the enum's member: an anonymous subclass of the
/// enum, so the override is a `Method` of `Color` too.
#[test]
fn test_parse_file_kotlin_enum_entry_body_fun_is_method_owned_by_enum() {
    let lg = parse(&kotlin(), "Color.kt", KOTLIN_ENUM);
    assert_at(&lg, "describe", 3, NodeKind::Method, Some("Color"));
}

#[test]
fn test_parse_file_kotlin_enum_companion_and_property_owned_by_enum() {
    let lg = parse(&kotlin(), "Color.kt", KOTLIN_ENUM);
    assert_owner(&lg, "parse", NodeKind::Method, "Color");
    assert_owner(&lg, "code", NodeKind::Property, "Color");
    assert_owner(&lg, "RED", NodeKind::EnumVariant, "Color");
}

#[test]
fn test_parse_file_kotlin_enum_nested_in_class_owns_its_fun() {
    let lg = parse(&kotlin(), "Color.kt", KOTLIN_ENUM);
    assert_owner(&lg, "m", NodeKind::Method, "In");
    assert_owner(&lg, "In", NodeKind::Enum, "Outer");
}

/// The two `describe` of one owner are overloads, kept as two nodes.
#[test]
fn test_build_kotlin_enum_and_entry_describe_keeps_both() {
    let graph = build(&kotlin(), "Color.kt", KOTLIN_ENUM);
    let rows: Vec<u32> = (0..graph.nodes.len() as u32)
        .filter(|&i| node_name(&graph, i) == "describe" && node_owner(&graph, i) == "Color")
        .map(|i| graph.nodes[i as usize].span.0)
        .collect();
    assert_eq!(rows.len(), 2, "entry override and enum member: {rows:?}");
}

#[test]
fn test_parse_file_dart_enhanced_enum_members_owned_by_enum() {
    let lg = parse(
        &dart(),
        "lib/color.dart",
        "enum Color {\n\
           red,\n\
           green;\n\
           String describe() => 'c';\n\
         }\n\
         enum Planet {\n\
           earth(1.0);\n\
           const Planet(this.mass);\n\
           final double mass;\n\
           double weight() => mass;\n\
         }\n\
         class Box {\n\
           String describe() => 'b';\n\
         }\n",
    );
    assert_at(&lg, "describe", 3, NodeKind::Method, Some("Color"));
    assert_at(&lg, "describe", 12, NodeKind::Method, Some("Box"));
    assert_owner(&lg, "Planet", NodeKind::Constructor, "Planet");
    assert_owner(&lg, "mass", NodeKind::Property, "Planet");
    assert_owner(&lg, "weight", NodeKind::Method, "Planet");
    assert_owner(&lg, "red", NodeKind::EnumVariant, "Color");
}

#[test]
fn test_parse_file_php_enum_method_owned_by_enum() {
    let lg = parse(
        &php(),
        "src/Suit.php",
        "<?php\n\
         enum Suit: string {\n\
             case Hearts = 'H';\n\
             public function color(): string { return 'R'; }\n\
         }\n",
    );
    assert_owner(&lg, "color", NodeKind::Method, "Suit");
    assert_owner(&lg, "Hearts", NodeKind::EnumVariant, "Suit");
}

/// Swift's own `stamp_member_owners` runs first; the shared stamp only
/// fills what it left, so the two agree.
#[test]
fn test_parse_file_swift_enum_with_associated_values_members_owned_by_enum() {
    let lg = parse(
        &swift(),
        "Sources/Shape.swift",
        "enum Shape {\n\
             case circle(r: Double), square(Double)\n\
             func area() -> Double { return 0 }\n\
             var name: String { return \"s\" }\n\
             struct Corner {\n\
                 func at() -> Int { return 0 }\n\
             }\n\
         }\n",
    );
    assert_owner(&lg, "area", NodeKind::Method, "Shape");
    assert_owner(&lg, "name", NodeKind::Property, "Shape");
    assert_owner(&lg, "circle", NodeKind::EnumVariant, "Shape");
    assert_owner(&lg, "at", NodeKind::Method, "Corner");
    assert_owner(&lg, "Corner", NodeKind::Struct, "Shape");
}

#[test]
fn test_parse_file_csharp_enum_in_class_owned_by_class() {
    let lg = parse(
        &csharp(),
        "Box.cs",
        "namespace N {\n\
             enum Color { Red, Green }\n\
             class Box {\n\
                 enum Inner { A }\n\
                 void Describe() {}\n\
             }\n\
         }\n",
    );
    assert_owner(&lg, "Red", NodeKind::EnumVariant, "Color");
    assert_owner(&lg, "A", NodeKind::EnumVariant, "Inner");
    assert_owner(&lg, "Inner", NodeKind::Enum, "Box");
    assert_owner(&lg, "Describe", NodeKind::Method, "Box");
}

#[test]
fn test_parse_file_typescript_enum_members_and_class_method_keep_owners() {
    let lg = parse(
        &typescript(),
        "src/color.ts",
        "enum Color { Red = 1, Green = 2 }\n\
         class Box {\n\
           describe() { return 1; }\n\
         }\n",
    );
    assert_owner(&lg, "Red", NodeKind::EnumVariant, "Color");
    assert_owner(&lg, "describe", NodeKind::Method, "Box");
}

/// JavaScript has no enum: the class member is the nearest analogue.
#[test]
fn test_parse_file_javascript_class_method_keeps_class_owner() {
    let lg = parse(
        &javascript(),
        "src/box.js",
        "class Box {\n\
           describe() { return 1; }\n\
         }\n",
    );
    assert_owner(&lg, "describe", NodeKind::Method, "Box");
}

/// A Python enum is a class: its method keeps the class as owner.
#[test]
fn test_parse_file_python_enum_class_method_owned_by_class() {
    let lg = parse(
        &python(),
        "color.py",
        "from enum import Enum\n\
         class Color(Enum):\n\
         \x20   RED = 1\n\
         \x20   def describe(self):\n\
         \x20       return 'c'\n",
    );
    assert_owner(&lg, "describe", NodeKind::Method, "Color");
}

/// Ruby has no enum: a constant-holding module is the nearest analogue.
#[test]
fn test_parse_file_ruby_module_method_owned_by_module() {
    let lg = parse(
        &ruby(),
        "color.rb",
        "module Color\n  RED = 1\n  def self.describe; end\nend\n",
    );
    assert_owner(&lg, "describe", NodeKind::Method, "Color");
}

/// Go has no enum: a defined type with a method is the nearest analogue.
#[test]
fn test_parse_file_go_defined_type_method_owned_by_receiver_type() {
    let lg = parse(
        &go(),
        "color.go",
        "package p\n\
         type Color int\n\
         const Red Color = 0\n\
         func (c Color) Describe() string { return \"\" }\n",
    );
    assert_owner(&lg, "Describe", NodeKind::Method, "Color");
}

/// C enums have no members: the enumerators keep the enum, a struct field
/// its struct.
#[test]
fn test_parse_file_c_enum_variants_and_struct_field_keep_owners() {
    let lg = parse(
        &c(),
        "mode.c",
        "enum Mode { ON, OFF };\nstruct S { int x; };\n",
    );
    assert_owner(&lg, "ON", NodeKind::EnumVariant, "Mode");
    assert_owner(&lg, "x", NodeKind::Property, "S");
}

/// A C++ `enum class` has no methods; one nested in a class is the class's.
#[test]
fn test_parse_file_cpp_enum_class_nested_in_class_keeps_owners() {
    let lg = parse(
        &cpp(),
        "mode.cpp",
        "enum class Mode { On, Off };\n\
         class C {\n\
           enum class In { A };\n\
           void m();\n\
         };\n",
    );
    assert_owner(&lg, "On", NodeKind::EnumVariant, "Mode");
    assert_owner(&lg, "A", NodeKind::EnumVariant, "In");
    assert_owner(&lg, "In", NodeKind::Enum, "C");
    assert_owner(&lg, "m", NodeKind::Method, "C");
}

/// Rust enum methods live in `impl`; struct-variant fields keep the enum.
#[test]
fn test_parse_file_rust_enum_impl_method_and_variant_field_owned_by_enum() {
    let lg = parse(
        &rust(),
        "src/shape.rs",
        "pub enum Shape { A { w: u8 }, B }\n\
         impl Shape { fn area(&self) -> u8 { 0 } }\n",
    );
    assert_owner(&lg, "area", NodeKind::Method, "Shape");
    assert_owner(&lg, "w", NodeKind::Property, "Shape");
    assert_owner(&lg, "A", NodeKind::EnumVariant, "Shape");
}

// ---- C++ namespace-qualified definitions ----

const CPP_QUALIFIED: &str = "namespace ns { void f(); }\n\
void ns::f() {}\n\
namespace a { namespace b { void h(); } }\n\
void a::b::h() {}\n\
namespace c::d { void k(); }\n\
void c::d::k() {}\n\
struct S { void m(); };\n\
void S::m() {}\n\
void Unknown::u() {}\n";

#[test]
fn test_parse_file_cpp_namespace_qualified_definition_is_function_of_namespace() {
    let lg = parse(&cpp(), "src/ns.cpp", CPP_QUALIFIED);
    assert_at(&lg, "f", 0, NodeKind::Function, Some("ns"));
    assert_at(&lg, "f", 1, NodeKind::Function, Some("ns"));
}

#[test]
fn test_parse_file_cpp_nested_namespace_qualified_definition_is_function_of_inner_namespace() {
    let lg = parse(&cpp(), "src/ns.cpp", CPP_QUALIFIED);
    assert_at(&lg, "h", 2, NodeKind::Function, Some("b"));
    assert_at(&lg, "h", 3, NodeKind::Function, Some("b"));
}

#[test]
fn test_parse_file_cpp_nested_namespace_specifier_qualified_definition_is_function_of_namespace() {
    let lg = parse(&cpp(), "src/ns.cpp", CPP_QUALIFIED);
    assert_at(&lg, "k", 4, NodeKind::Function, Some("c::d"));
    assert_at(&lg, "k", 5, NodeKind::Function, Some("c::d"));
}

#[test]
fn test_parse_file_cpp_class_qualified_definition_stays_method_of_class() {
    let lg = parse(&cpp(), "src/ns.cpp", CPP_QUALIFIED);
    assert_at(&lg, "m", 7, NodeKind::Method, Some("S"));
}

/// A qualifier the file declares neither as a class nor as a namespace
/// (a class from a header) stays an ownerless `Method`.
#[test]
fn test_parse_file_cpp_unknown_qualifier_definition_stays_ownerless_method() {
    let lg = parse(&cpp(), "src/ns.cpp", CPP_QUALIFIED);
    assert_at(&lg, "u", 8, NodeKind::Method, None);
}
