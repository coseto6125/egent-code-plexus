//! What a same-file pick and a call site's target kind take an owner to be.
//!
//! - Namespace owners: C++ declares `namespace detail { void to_json(); }`
//!   with owner `detail` and C# a namespace's classes with owner `N`. Such
//!   a declaration is free, not a member of an unrelated type `detail`, so
//!   a call that the file's types leave ambiguous binds it
//!   (`SymbolTable::build_owner_kinds`).
//! - Rust `impl` blocks: a member of `impl Trait for Cursor<T>` has no owner,
//!   and a member of `impl Trait for Foreign` an owner the index cannot
//!   resolve. An untyped `ty.m()` still reaches the trait declaration, and
//!   `self.m()` inside such a block reaches the block's own `m`
//!   (`SymbolTable::note_impl_member`).
//! - Member calls: `x.m()` / `x->m()` reaches a method or a function, never
//!   a type or a constructor by the type's name (`Resolver::resolve_call`).
//!   A plain construction `T()` / `new T()` still binds `T`; every one of
//!   the 14 languages has that guard, and the member-call case where the
//!   language can write it.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build(files: &[(&dyn LanguageProvider, &str, &str)]) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    for &(provider, path, source) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), source.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

/// One `Calls` target: kind, owner (`""` for none), file, 0-based row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    kind: NodeKind,
    owner: String,
    file: String,
    row: u32,
}

fn node_name(graph: &ZeroCopyGraph, id: u32) -> &str {
    graph.nodes[id as usize].name.resolve(&graph.string_pool)
}

/// The ids of the nodes named `name`, in id order.
fn nodes_named(graph: &ZeroCopyGraph, name: &str) -> Vec<u32> {
    (0..graph.nodes.len() as u32)
        .filter(|&i| node_name(graph, i) == name)
        .collect()
}

fn target(graph: &ZeroCopyGraph, id: u32) -> Target {
    let pool = &graph.string_pool;
    let node = &graph.nodes[id as usize];
    Target {
        kind: node.kind,
        owner: node.owner_class.resolve(pool).to_string(),
        file: graph.files[node.file_idx as usize]
            .path
            .resolve(pool)
            .to_string(),
        row: node.span.0,
    }
}

/// The `Calls` targets named `callee` of the caller node `caller`.
fn calls_of(graph: &ZeroCopyGraph, caller: u32, callee: &str) -> Vec<Target> {
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == caller)
        .filter(|e| node_name(graph, e.target) == callee)
        .map(|e| target(graph, e.target))
        .collect()
}

/// The `Calls` targets named `callee` of the one node named `caller`.
fn calls_from(graph: &ZeroCopyGraph, caller: &str, callee: &str) -> Vec<Target> {
    let callers = nodes_named(graph, caller);
    assert_eq!(callers.len(), 1, "one `{caller}` caller");
    calls_of(graph, callers[0], callee)
}

fn assert_binds_owner(graph: &ZeroCopyGraph, caller: &str, callee: &str, owner: &str) {
    let targets = calls_from(graph, caller, callee);
    assert_eq!(
        targets.len(),
        1,
        "`{caller}` calls one `{callee}`: {targets:?}"
    );
    assert_eq!(
        targets[0].owner, owner,
        "`{caller}` binds {owner:?}'s `{callee}`"
    );
}

/// `caller` calls nothing named `callee` that is a type or a constructor.
fn assert_binds_no_type(graph: &ZeroCopyGraph, caller: &str, callee: &str) {
    let targets = calls_from(graph, caller, callee);
    assert!(
        targets
            .iter()
            .all(|t| matches!(t.kind, NodeKind::Function | NodeKind::Method)),
        "a member call `{caller}` → `{callee}` binds no type: {targets:?}"
    );
}

/// `caller` calls `callee`, and every target is a type or its constructor.
fn assert_binds_type(graph: &ZeroCopyGraph, caller: &str, callee: &str) {
    let targets = calls_from(graph, caller, callee);
    assert!(!targets.is_empty(), "`{caller}` constructs `{callee}`");
    assert!(
        targets
            .iter()
            .all(|t| t.kind.is_constructible() || t.kind == NodeKind::Constructor),
        "`{caller}` → `{callee}` binds the type: {targets:?}"
    );
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

// ---- namespace owners count as no owner ----

#[test]
fn test_resolve_call_cpp_namespace_function_beside_member_binds_namespace_function() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "class A { public: void to_json(int x); };\n\
         namespace detail { void to_json(int x) {} }\n\
         class B { void run() { to_json(1); } };\n",
    )]);
    assert_binds_owner(&graph, "run", "to_json", "detail");
}

#[test]
fn test_resolve_call_cpp_nested_namespace_function_binds_namespace_function() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "class A { public: void to_json(int x); };\n\
         namespace a { namespace b { void to_json(int x) {} } }\n\
         class B { void run() { to_json(1); } };\n",
    )]);
    assert_binds_owner(&graph, "run", "to_json", "b");
}

#[test]
fn test_resolve_call_cpp_anonymous_namespace_function_binds_free_function() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "class A { public: void to_json(int x); };\n\
         namespace { void to_json(int x) {} }\n\
         class B { void run() { to_json(1); } };\n",
    )]);
    assert_binds_owner(&graph, "run", "to_json", "");
}

/// A type `detail` in another file does not make the namespace `detail`
/// of this file a type: the rule is per file.
#[test]
fn test_resolve_call_cpp_namespace_named_like_type_of_other_file_binds_namespace_function() {
    let graph = build(&[
        (
            &cpp(),
            "a.cpp",
            "class A { public: void to_json(int x); };\n\
             namespace detail { void to_json(int x) {} }\n\
             class B { void run() { to_json(1); } };\n",
        ),
        (
            &cpp(),
            "b.cpp",
            "class detail { public: void to_json(int x); };\n",
        ),
    ]);
    let targets = calls_from(&graph, "run", "to_json");
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(
        (targets[0].owner.as_str(), targets[0].file.as_str()),
        ("detail", "a.cpp")
    );
}

/// A file that declares both a namespace and a type `detail` keeps
/// `detail` a type: two unrelated owners, so no edge.
#[test]
fn test_resolve_call_cpp_namespace_and_type_of_one_name_in_one_file_binds_nothing() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "class A { public: void to_json(int x); };\n\
         namespace detail { void helper() {} }\n\
         class detail { public: void to_json(int x); };\n\
         class B { void run() { to_json(1); } };\n",
    )]);
    let targets = calls_from(&graph, "run", "to_json");
    assert!(targets.is_empty(), "two unrelated types: {targets:?}");
}

/// C# gives a namespace's classes the namespace as owner: inside `N`,
/// `new Inner()` reaches `N.Inner`, not the class nested in `Outer`.
#[test]
fn test_resolve_call_csharp_namespace_class_beside_nested_class_binds_namespace_class() {
    let graph = build(&[(
        &csharp(),
        "a.cs",
        "namespace N {\n\
         class Outer { class Inner {} }\n\
         class Inner {}\n\
         class User { void Run() { new Inner(); } }\n\
         }\n",
    )]);
    let targets = calls_from(&graph, "Run", "Inner");
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(
        (targets[0].kind, targets[0].owner.as_str()),
        (NodeKind::Class, "N")
    );
}

/// PHP gives namespace functions no owner; the rule leaves them free.
#[test]
fn test_resolve_call_php_namespace_function_beside_member_binds_free_function() {
    let graph = build(&[(
        &php(),
        "a.php",
        "<?php\nnamespace App;\n\
         class A { function to_json() {} }\n\
         function to_json() {}\n\
         function run() { to_json(); }\n",
    )]);
    assert_binds_owner(&graph, "run", "to_json", "");
}

/// A Kotlin `object` is a type, and a `package` declares no owner: two
/// unrelated types still bind nothing.
#[test]
fn test_resolve_call_kotlin_object_member_stays_type_owner_binds_nothing() {
    let graph = build(&[(
        &kotlin(),
        "a.kt",
        "package a.b\n\
         class A { fun save() {} }\n\
         object b { fun save() {} }\n\
         fun run() { save() }\n",
    )]);
    let targets = calls_from(&graph, "run", "save");
    assert!(targets.is_empty(), "two unrelated types: {targets:?}");
}

/// A Ruby module is a type owner (the parser emits it as a Trait), so its
/// method is no free function.
#[test]
fn test_resolve_call_ruby_module_method_stays_type_owner_binds_nothing() {
    let graph = build(&[(
        &ruby(),
        "a.rb",
        "class A\n  def save; end\nend\n\
         module Greet\n  def save; end\nend\n\
         def run\n  save()\nend\n",
    )]);
    let targets = calls_from(&graph, "run", "save");
    assert!(targets.is_empty(), "two unrelated types: {targets:?}");
}

// ---- Rust impl blocks ----

const MOVE_LAYOUT: &str = "pub trait MoveLayout {\n\
    fn write_name(&self, out: &mut String);\n\
    fn get_layout(&self) -> u32;\n\
}\n\
impl MoveLayout for MoveFieldLayout {\n\
    fn write_name(&self, out: &mut String) {}\n\
    fn get_layout(&self) -> u32 { 0 }\n\
}\n\
impl MoveLayout for Vec<u8> {\n\
    fn write_name(&self, out: &mut String) {}\n\
    fn get_layout(&self) -> u32 { 1 }\n\
}\n\
fn print(ty: &impl MoveLayout, out: &mut String) {\n\
    ty.write_name(out);\n\
    ty.get_layout();\n\
}\n";

#[test]
fn test_resolve_call_rust_trait_and_foreign_impls_untyped_receiver_binds_trait_declaration() {
    let graph = build(&[(&rust(), "src/lib.rs", MOVE_LAYOUT)]);
    assert_binds_owner(&graph, "print", "write_name", "MoveLayout");
    assert_binds_owner(&graph, "print", "get_layout", "MoveLayout");
}

/// Every `poll_shutdown` calls `self.poll_flush()` on its own line, and
/// each impl block sits on one line: the block's own `poll_flush` is the
/// one on the caller's row. The owners are generic (`Pin<P>`,
/// `Cursor<..>`) or scoped (`io::Cursor<..>`, which the parser emits no
/// `Impl` node for), so the members have no owner and share uids.
const ASYNC_WRITE: &str = "pub trait AsyncWrite {\n\
    fn poll_flush(&mut self) -> u32;\n\
    fn poll_shutdown(&mut self) -> u32;\n\
}\n\
impl<P> AsyncWrite for Pin<P> { fn poll_flush(&mut self) -> u32 { 0 } fn poll_shutdown(&mut self) -> u32 { self.poll_flush() } }\n\
impl AsyncWrite for io::Cursor<&mut [u8]> { fn poll_flush(&mut self) -> u32 { 1 } fn poll_shutdown(&mut self) -> u32 { self.poll_flush() } }\n\
impl AsyncWrite for io::Cursor<Vec<u8>> { fn poll_flush(&mut self) -> u32 { 2 } fn poll_shutdown(&mut self) -> u32 { self.poll_flush() } }\n\
impl AsyncWrite for Cursor<Box<[u8]>> { fn poll_flush(&mut self) -> u32 { 3 } fn poll_shutdown(&mut self) -> u32 { self.poll_flush() } }\n\
impl AsyncWrite for Cursor<Vec<u16>> { fn poll_flush(&mut self) -> u32 { 4 } fn poll_shutdown(&mut self) -> u32 { self.poll_flush() } }\n";

#[test]
fn test_resolve_call_rust_self_call_in_generic_impl_binds_own_block_member() {
    let graph = build(&[(&rust(), "src/aw.rs", ASYNC_WRITE)]);
    let callers = nodes_named(&graph, "poll_shutdown");
    let impl_callers: Vec<u32> = callers
        .into_iter()
        .filter(|&id| target(&graph, id).owner.is_empty())
        .collect();
    assert_eq!(impl_callers.len(), 5, "five impl blocks");
    for caller in impl_callers {
        let row = target(&graph, caller).row;
        let targets = calls_of(&graph, caller, "poll_flush");
        assert_eq!(targets.len(), 1, "row {row}: {targets:?}");
        assert_eq!(
            targets[0].row, row,
            "row {row} binds its own block's `poll_flush`"
        );
    }
}

/// An inherent impl with a nameable owner types `self`: the qualifier tier
/// binds the owner's member, as before.
#[test]
fn test_resolve_call_rust_self_call_in_named_impl_binds_owner_member() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub trait AsyncWrite { fn poll_flush(&mut self) -> u32; }\n\
         impl<P> AsyncWrite for Pin<P> { fn poll_flush(&mut self) -> u32 { 0 } }\n\
         struct Wrapper;\n\
         impl AsyncWrite for Wrapper { fn poll_flush(&mut self) -> u32 { 1 } }\n\
         impl Wrapper { fn close(&mut self) -> u32 { self.poll_flush() } }\n",
    )]);
    assert_binds_owner(&graph, "close", "poll_flush", "Wrapper");
}

#[test]
fn test_resolve_call_rust_trait_default_method_and_foreign_impl_binds_trait_declaration() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub trait T { fn m(&self) {} }\n\
         impl T for Vec<u8> { fn m(&self) {} }\n\
         fn run(x: &dyn T) { x.m(); }\n",
    )]);
    assert_binds_owner(&graph, "run", "m", "T");
}

#[test]
fn test_resolve_call_rust_blanket_impl_binds_trait_declaration() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub trait T { fn m(&self); }\n\
         impl<X: Clone> T for X { fn m(&self) {} }\n\
         impl T for Vec<u8> { fn m(&self) {} }\n\
         fn run(x: &dyn T) { x.m(); }\n",
    )]);
    assert_binds_owner(&graph, "run", "m", "T");
}

/// `S` implements two traits that both declare `m`: no root among them,
/// so an untyped `x.m()` binds nothing.
#[test]
fn test_resolve_call_rust_two_traits_with_one_member_name_binds_nothing() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub trait A { fn m(&self); }\n\
         pub trait B { fn m(&self); }\n\
         pub struct S;\n\
         impl A for S { fn m(&self) {} }\n\
         impl B for S { fn m(&self) {} }\n\
         fn run(x: &dyn std::any::Any) { x.m(); }\n",
    )]);
    let targets = calls_from(&graph, "run", "m");
    assert!(targets.is_empty(), "two unrelated traits: {targets:?}");
}

/// A `fn` that shares its own name never binds itself through its block:
/// `Pin::new(..).poll_flush()` inside `poll_flush` goes elsewhere.
#[test]
fn test_resolve_call_rust_call_of_own_name_in_generic_impl_binds_trait_declaration() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub trait AsyncWrite { fn poll_flush(&mut self) -> u32; }\n\
         impl<P> AsyncWrite for Pin<P> { fn poll_flush(&mut self) -> u32 { Pin::new(self).poll_flush() } }\n\
         impl AsyncWrite for Vec<u8> { fn poll_flush(&mut self) -> u32 { 1 } }\n",
    )]);
    let caller = nodes_named(&graph, "poll_flush")
        .into_iter()
        .find(|&id| target(&graph, id).row == 1)
        .expect("the Pin impl's `poll_flush`");
    let targets = calls_of(&graph, caller, "poll_flush");
    assert!(
        targets.iter().all(|t| t.owner == "AsyncWrite"),
        "the trait declaration or nothing: {targets:?}"
    );
}

// ---- member calls never bind a type ----

/// nlohmann's `sax->number_integer(v)`: the receiver is a field the parser
/// cannot type, two unrelated sax classes declare the member, and a test
/// file declares `template <typename T> class number_integer`.
#[test]
fn test_resolve_call_cpp_member_call_named_like_class_of_other_file_binds_no_type() {
    let graph = build(&[
        (
            &cpp(),
            "src/reader.hpp",
            "class sax_a { public: bool number_integer(int v) { return true; } };\n\
             class sax_b { public: bool number_integer(int v) { return false; } };\n\
             template<typename SAX>\n\
             class binary_reader {\n\
                 bool parse_msgpack_internal() { int x = 1; return sax->number_integer(x); }\n\
                 SAX* sax = nullptr;\n\
             };\n",
        ),
        (
            &cpp(),
            "tests/regress.cpp",
            "template <typename T> class number_integer {};\n",
        ),
    ]);
    assert_binds_no_type(&graph, "parse_msgpack_internal", "number_integer");
}

#[test]
fn test_resolve_call_cpp_plain_construction_binds_class() {
    let graph = build(&[(
        &cpp(),
        "tests/regress.cpp",
        "template <typename T> class number_integer {};\n\
         void make() { number_integer(1); }\n",
    )]);
    assert_binds_type(&graph, "make", "number_integer");
}

/// Needs the C++ parser to write an untyped `h.w->Widget(1)` as an untyped
/// member call; a bare `Widget` is a construction.
#[test]
fn test_resolve_call_cpp_member_call_named_like_constructor_binds_no_type() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "struct Widget { Widget(int x) {} };\n\
         struct Holder { void* w; };\n\
         class User { void run(Holder h) { h.w->Widget(1); } };\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
}

#[test]
fn test_resolve_call_java_computed_receiver_named_like_constructor_binds_no_type() {
    let graph = build(&[
        (&java(), "Foo.java", "class Foo { Foo(int x) {} }\n"),
        (
            &java(),
            "User.java",
            "class User { Object make() { return null; } void run() { make().Foo(1); } }\n",
        ),
    ]);
    assert_binds_no_type(&graph, "run", "Foo");
}

#[test]
fn test_resolve_call_java_new_binds_constructor() {
    let graph = build(&[
        (&java(), "Foo.java", "class Foo { Foo(int x) {} }\n"),
        (
            &java(),
            "User.java",
            "class User { void mk() { new Foo(1); } }\n",
        ),
    ]);
    assert_binds_type(&graph, "mk", "Foo");
}

#[test]
fn test_resolve_call_csharp_member_call_named_like_constructor_binds_no_type() {
    let graph = build(&[(
        &csharp(),
        "a.cs",
        "class Widget { public Widget(int x) {} }\n\
         class User { void Run(dynamic x) { x.Widget(1); } void Mk() { new Widget(1); } }\n",
    )]);
    assert_binds_no_type(&graph, "Run", "Widget");
    assert_binds_type(&graph, "Mk", "Widget");
}

#[test]
fn test_resolve_call_typescript_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &typescript(),
        "a.ts",
        "class Widget {}\n\
         class User { run(x: any) { x.Widget(); } make() { new Widget(); } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_javascript_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &javascript(),
        "a.js",
        "class Widget {}\n\
         class User { run(x) { x.Widget(); } make() { new Widget(); } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_python_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &python(),
        "a.py",
        "class Widget:\n    pass\n\n\
         class User:\n    def run(self, x):\n        x.Widget()\n\n    def make(self):\n        Widget()\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_kotlin_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &kotlin(),
        "a.kt",
        "class Widget {}\n\
         class User { fun run(x: Any) { x.Widget() } fun make() { Widget() } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_swift_member_call_named_like_struct_binds_no_type() {
    let graph = build(&[(
        &swift(),
        "a.swift",
        "struct Widget {}\n\
         class User { func run(x: Any) { x.Widget() } func make() { Widget() } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

/// The Swift parser may write an untyped `x.widget()` as a bare `widget`;
/// two unrelated classes declare the member, so the name is a member's
/// and never constructs the struct `widget` of another file.
#[test]
fn test_resolve_call_swift_member_call_beside_two_members_binds_no_type() {
    let graph = build(&[
        (
            &swift(),
            "Sources/a.swift",
            "class A { func widget() {} }\n\
             class B { func widget() {} }\n\
             class User { func run(x: Any) { x.widget() } }\n",
        ),
        (&swift(), "Tests/b.swift", "struct widget {}\n"),
    ]);
    assert_binds_no_type(&graph, "run", "widget");
}

#[test]
fn test_resolve_call_dart_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &dart(),
        "a.dart",
        "class Widget {}\n\
         class User { void run(dynamic x) { x.Widget(); } void make() { Widget(); } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_php_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &php(),
        "a.php",
        "<?php\nclass Widget {}\n\
         class User { function run($x) { $x->Widget(); } function make() { new Widget(); } }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

#[test]
fn test_resolve_call_ruby_member_call_named_like_class_binds_no_type() {
    let graph = build(&[(
        &ruby(),
        "a.rb",
        "class Widget; end\n\
         class User\n  def run(x); x.Widget(); end\n  def make; Widget.new; end\nend\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
    assert_binds_type(&graph, "make", "Widget");
}

/// Go has no constructors: a member call named like a struct binds no type.
#[test]
fn test_resolve_call_go_member_call_named_like_struct_binds_no_type() {
    let graph = build(&[(
        &go(),
        "a.go",
        "package p\n\
         type Widget struct{}\n\
         func run(x interface{ Widget() }) { x.Widget() }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
}

/// Rust has no constructors: a method call named like a struct binds no
/// type.
#[test]
fn test_resolve_call_rust_method_call_named_like_struct_binds_no_type() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub struct Widget;\n\
         fn run(x: &dyn std::any::Any) { x.Widget(); }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
}

/// C has no constructors: a function-pointer member call named like a
/// struct binds no type.
#[test]
fn test_resolve_call_c_member_call_named_like_struct_binds_no_type() {
    let graph = build(&[(
        &c(),
        "a.c",
        "struct Widget { int x; };\n\
         struct Holder { void (*Widget)(int); };\n\
         void run(struct Holder *h) { h->Widget(1); }\n",
    )]);
    assert_binds_no_type(&graph, "run", "Widget");
}

// ---- untyped receivers: never a free function, never narrowed by the caller ----

#[test]
fn test_resolve_call_python_untyped_receiver_with_only_free_function_binds_nothing() {
    let graph = build(&[(
        &python(),
        "a.py",
        "def size():\n    pass\n\ndef run(f):\n    f.size()\n",
    )]);
    let targets = calls_from(&graph, "run", "size");
    assert!(
        targets.is_empty(),
        "an untyped `f.size()` binds no free function: {targets:?}"
    );
}

#[test]
fn test_resolve_call_rust_untyped_receiver_with_only_free_function_binds_nothing() {
    let graph = build(&[(
        &rust(),
        "src/lib.rs",
        "pub fn size() -> u32 { 0 }\n\
         fn run() { make().size(); }\n",
    )]);
    let targets = calls_from(&graph, "run", "size");
    assert!(
        targets.is_empty(),
        "an untyped `f.size()` binds no free function: {targets:?}"
    );
}

/// `o.save()` in `C`, which declares its own `save`, beside an unrelated
/// `A.save`: the receiver is no `self`, so the caller's type does not pick.
#[test]
fn test_resolve_call_python_untyped_receiver_in_class_with_own_member_binds_nothing() {
    let graph = build(&[(
        &python(),
        "a.py",
        "class A:\n    def save(self):\n        pass\n\n\
         class C:\n    def save(self):\n        pass\n\n    def run(self, o):\n        o.save()\n",
    )]);
    let targets = calls_from(&graph, "run", "save");
    assert!(targets.is_empty(), "two candidate types: {targets:?}");
}

/// A free caller with one type declaring the member still binds it.
#[test]
fn test_resolve_call_python_untyped_receiver_one_other_owner_binds_its_member() {
    let graph = build(&[(
        &python(),
        "a.py",
        "def save():\n    pass\n\n\
         class A:\n    def save(self):\n        pass\n\n\
         def run(o):\n    o.save()\n",
    )]);
    assert_binds_owner(&graph, "run", "save", "A");
}

// ---- the caller's bases come from every heritage-bearing kind ----

#[test]
fn test_resolve_call_cpp_struct_with_base_binds_base_member() {
    let graph = build(&[(
        &cpp(),
        "a.cpp",
        "struct A { void save() {} };\n\
         struct B { void save() {} };\n\
         struct C : B { void run() { save(); } };\n",
    )]);
    assert_binds_owner(&graph, "run", "save", "B");
}

#[test]
fn test_resolve_call_kotlin_interface_extending_interface_binds_base_member() {
    let graph = build(&[(
        &kotlin(),
        "J.kt",
        "class A { fun save() {} }\n\
         interface I { fun save() {} }\n\
         interface J : I { fun run() { save() } }\n",
    )]);
    assert_binds_owner(&graph, "run", "save", "I");
}

#[test]
fn test_resolve_call_csharp_struct_implementing_interface_binds_interface_member() {
    let graph = build(&[(
        &csharp(),
        "C.cs",
        "class A { void Save() {} }\n\
         interface I { void Save() {} }\n\
         struct C : I { void Run() { Save(); } }\n",
    )]);
    assert_binds_owner(&graph, "Run", "Save", "I");
}

#[test]
fn test_resolve_call_ruby_module_including_module_binds_included_member() {
    let graph = build(&[(
        &ruby(),
        "m.rb",
        "class A\n  def save; end\nend\n\
         module Helpers\n  def save; end\nend\n\
         module M\n  include Helpers\n  def run\n    save()\n  end\nend\n",
    )]);
    assert_binds_owner(&graph, "run", "save", "Helpers");
}

/// The parser gives a Swift enum's members no owner; the enclosing enum
/// still supplies the bases.
#[test]
fn test_resolve_call_swift_enum_conforming_to_protocol_binds_protocol_member() {
    let graph = build(&[(
        &swift(),
        "C.swift",
        "class A { func save() {} }\n\
         protocol P { func save() }\n\
         enum C: P {\n    case x\n    func run() { save() }\n}\n",
    )]);
    assert_binds_owner(&graph, "run", "save", "P");
}
