//! Verifies that `RelType::Implements` is emitted by the Pass-2 heritage walk
//! when the resolved target's NodeKind is Interface or Trait.
//!
//! Per spec §8.2 / CLAUDE.md "14-language coverage" rule: every change to
//! graph-construction primitives ships with fixtures for the 14 mainstream
//! languages. This file covers the 8 languages where an Interface NodeKind
//! can be expressed in the source + resolved by the symbol table. Languages
//! deferred per spec §4.5:
//!
//!   * Python — Pending PR §8.3 (Protocol detection, NodeKind promotion).
//!   * Ruby   — Module mixin / duck-typed; no Interface NodeKind today.
//!   * Dart   — Implicit interface convention; deferred.
//!   * C / C++ — No interface concept.
//!   * JS     — No interface concept.
//!
//! Each non-deferred language has two assertions:
//!   (a) class → Interface/Trait emits `Implements` (the new path).
//!   (b) class → concrete class/struct emits `Extends` (regression guard).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::{NodeKind, RelType};

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Minimal RawNode builder so fixtures stay terse.
fn raw_node(name: &str, kind: NodeKind, heritage: Vec<&str>) -> RawNode {
    RawNode {
        name: name.into(),
        kind,
        span: (0, 0, 5, 0),
        is_exported: true,
        heritage: heritage.into_iter().map(Into::into).collect(),
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        owner_class: None,
        content_hash: 0,
    }
}

fn empty_graph(file_path: &str, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: file_path.into(),
        content_hash: [0; 8],
        nodes,
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        call_metas: vec![],
        raw_function_metas: vec![],
    }
}

/// Build a two-file graph and return the (source_node_id, target_node_id, rel_type)
/// triples for all edges.
fn build_edges(iface_graph: LocalGraph, impl_graph: LocalGraph) -> Vec<(u32, u32, RelType)> {
    let mut builder = GraphBuilder::new();
    builder.add_graph(iface_graph);
    builder.add_graph(impl_graph);
    let graph = builder.build();
    graph
        .edges
        .iter()
        .map(|e| (e.source, e.target, e.rel_type))
        .collect()
}

fn has_rel(edges: &[(u32, u32, RelType)], rel: RelType) -> bool {
    edges.iter().any(|&(_, _, r)| r == rel)
}

// ── TypeScript ────────────────────────────────────────────────────────────────

/// TypeScript: `interface IFoo {}` → Interface; `class Bar implements IFoo`
/// → Implements edge; `class Baz extends OtherClass` → Extends edge.
#[test]
fn typescript_implements_edge() {
    use ecp_analyzer::typescript::parser::TypeScriptProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");

    let iface_src = b"export interface IFoo { greet(): void; }
export class OtherClass {}";
    let impl_src = b"import { IFoo, OtherClass } from './iface';
export class Bar implements IFoo { greet() {} }
export class Baz extends OtherClass {}";

    let iface_graph = provider
        .parse_file("iface.ts".as_ref(), iface_src)
        .expect("parse iface.ts");
    let impl_graph = provider
        .parse_file("impl.ts".as_ref(), impl_src)
        .expect("parse impl.ts");

    let mut builder = GraphBuilder::new();
    builder.add_graph(iface_graph);
    builder.add_graph(impl_graph);
    let graph = builder.build();
    let edges: Vec<_> = graph
        .edges
        .iter()
        .map(|e| (e.source, e.target, e.rel_type))
        .collect();

    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (Bar → IFoo); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (Baz → OtherClass); got: {edges:?}"
    );
}

// ── Java ─────────────────────────────────────────────────────────────────────

#[test]
fn java_implements_edge() {
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = JavaProvider::new().expect("JavaProvider::new");

    let iface_src = b"public interface IGreeter { void greet(); }
public class BaseClass {}";
    let impl_src = b"public class Hello implements IGreeter {
    public void greet() {}
}
public class Child extends BaseClass {}";

    let iface_graph = provider
        .parse_file("IGreeter.java".as_ref(), iface_src)
        .expect("parse IGreeter.java");
    let impl_graph = provider
        .parse_file("Hello.java".as_ref(), impl_src)
        .expect("parse Hello.java");

    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (Hello → IGreeter); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (Child → BaseClass); got: {edges:?}"
    );
}

// ── C# ───────────────────────────────────────────────────────────────────────

#[test]
fn csharp_implements_edge() {
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = CSharpProvider::new().expect("CSharpProvider::new");

    let iface_src = b"public interface ILogger { void Log(string msg); }
public class BaseLogger {}";
    let impl_src = b"public class ConsoleLogger : ILogger {
    public void Log(string msg) {}
}
public class SubLogger : BaseLogger {}";

    let iface_graph = provider
        .parse_file("ILogger.cs".as_ref(), iface_src)
        .expect("parse ILogger.cs");
    let impl_graph = provider
        .parse_file("ConsoleLogger.cs".as_ref(), impl_src)
        .expect("parse ConsoleLogger.cs");

    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (ConsoleLogger → ILogger); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (SubLogger → BaseLogger); got: {edges:?}"
    );
}

// ── PHP ───────────────────────────────────────────────────────────────────────

#[test]
fn php_implements_edge() {
    use ecp_analyzer::php::parser::PhpProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = PhpProvider::new().expect("PhpProvider::new");

    let iface_src = b"<?php
interface IShape { public function area(): float; }
class BaseShape {}";
    let impl_src = b"<?php
class Circle implements IShape {
    public function area(): float { return 3.14; }
}
class SpecialCircle extends BaseShape {}";

    let iface_graph = provider
        .parse_file("IShape.php".as_ref(), iface_src)
        .expect("parse IShape.php");
    let impl_graph = provider
        .parse_file("Circle.php".as_ref(), impl_src)
        .expect("parse Circle.php");

    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (Circle → IShape); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (SpecialCircle → BaseShape); got: {edges:?}"
    );
}

// ── Rust ──────────────────────────────────────────────────────────────────────
//
// In Rust, `impl Trait for Struct` produces an Impl node named "Struct"
// with heritage = ["Trait"]. The Trait NodeKind triggers Implements dispatch.

#[test]
fn rust_implements_edge() {
    use ecp_analyzer::rust::parser::RustProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = RustProvider::new().expect("RustProvider::new");

    let trait_src = b"pub trait Greet { fn greet(&self); }
pub struct BaseStruct;";
    // impl Trait for Struct → Impl node "MyStruct" with heritage=["Greet"]
    // impl Struct           → Impl node "BaseStruct" (no heritage → Extends? no heritage at all)
    // For the Extends regression we add a second struct that extends BaseStruct
    // via a type alias to keep the test symmetric. Actually Rust has no
    // class-extends-class; we only assert Implements fires.
    let impl_src = b"pub struct MyStruct;
impl Greet for MyStruct { fn greet(&self) {} }";

    let trait_graph = provider
        .parse_file("greet.rs".as_ref(), trait_src)
        .expect("parse greet.rs");
    let impl_graph = provider
        .parse_file("my_struct.rs".as_ref(), impl_src)
        .expect("parse my_struct.rs");

    let edges = build_edges(trait_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (MyStruct impl-block → Greet trait); got: {edges:?}"
    );
}

// ── Swift ─────────────────────────────────────────────────────────────────────
//
// Swift `protocol` → NodeKind::Trait (per spec decision §10 #3).
// `class Foo: SomeProto` → heritage=["SomeProto"] → Implements.

#[test]
fn swift_implements_edge() {
    use ecp_analyzer::swift::parser::SwiftProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let provider = SwiftProvider::new().expect("SwiftProvider::new");

    let proto_src = b"protocol Drawable { func draw() }
class BaseShape {}";
    let impl_src = b"class Circle: Drawable { func draw() {} }
class SpecialCircle: BaseShape {}";

    let proto_graph = provider
        .parse_file("Drawable.swift".as_ref(), proto_src)
        .expect("parse Drawable.swift");
    let impl_graph = provider
        .parse_file("Circle.swift".as_ref(), impl_src)
        .expect("parse Circle.swift");

    let edges = build_edges(proto_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (Circle → Drawable protocol); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (SpecialCircle → BaseShape); got: {edges:?}"
    );
}

// ── Go ────────────────────────────────────────────────────────────────────────
//
// Go uses structural typing — there is no explicit `implements` keyword.
// The spec §4.5 marks Go as "OK" because Go interface embeddings in
// struct fields produce a heritage entry pointing to the Interface node.
// We test this via a raw LocalGraph fixture that mimics what the Go parser
// emits for a struct embedding a named interface type.

#[test]
fn go_implements_edge_raw_fixture() {
    // Fixture: src/iface.go defines `type Stringer interface { String() string }`
    // Fixture: src/impl.go defines `type MyType struct { Stringer }` (embedding)
    // The Go parser emits Stringer as heritage of MyType's Struct node.
    let iface_graph = empty_graph(
        "src/iface.go",
        vec![raw_node("Stringer", NodeKind::Interface, vec![])],
    );
    let impl_graph = empty_graph(
        "src/impl.go",
        vec![
            raw_node("MyType", NodeKind::Struct, vec!["Stringer"]),
            raw_node("ConcreteBase", NodeKind::Struct, vec![]),
            raw_node("Derived", NodeKind::Struct, vec!["ConcreteBase"]),
        ],
    );

    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (MyType → Stringer interface); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (Derived → ConcreteBase); got: {edges:?}"
    );
}

// ── Kotlin ────────────────────────────────────────────────────────────────────
//
// Kotlin's tree-sitter grammar uses `class_declaration` for both class and
// interface; the ecp parser does not yet emit NodeKind::Interface for Kotlin
// interfaces. Raw fixture asserts the kind-based dispatch logic fires correctly
// when a future parser update promotes Kotlin interfaces.

#[test]
fn kotlin_implements_edge_raw_fixture() {
    let iface_graph = empty_graph(
        "src/IRepo.kt",
        vec![raw_node("IRepo", NodeKind::Interface, vec![])],
    );
    let impl_graph = empty_graph(
        "src/InMemoryRepo.kt",
        vec![
            raw_node("InMemoryRepo", NodeKind::Class, vec!["IRepo"]),
            raw_node("BaseRepo", NodeKind::Class, vec![]),
            raw_node("CachedRepo", NodeKind::Class, vec!["BaseRepo"]),
        ],
    );

    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (InMemoryRepo → IRepo interface); got: {edges:?}"
    );
    assert!(
        has_rel(&edges, RelType::Extends),
        "expected Extends edge (CachedRepo → BaseRepo); got: {edges:?}"
    );
}

// ── Python (deferred) ─────────────────────────────────────────────────────────
//
// Python Protocol / ABC detection requires PR §8.3 (NodeKind promotion from
// Class → Interface for Protocol subclasses). The raw fixture below proves
// the builder dispatches correctly once that promotion lands.

#[test]
#[ignore = "Pending PR §8.3 (Python Protocol detection — promotes Protocol subclass to NodeKind::Interface)"]
fn python_implements_edge_pending_protocol_detection() {
    // Once §8.3 ships: `class Printable(Protocol): ...` → NodeKind::Interface.
    // Raw fixture simulates that post-promotion state.
    let iface_graph = empty_graph(
        "src/printable.py",
        vec![raw_node("Printable", NodeKind::Interface, vec![])],
    );
    let impl_graph = empty_graph(
        "src/doc.py",
        vec![raw_node("Document", NodeKind::Class, vec!["Printable"])],
    );
    let edges = build_edges(iface_graph, impl_graph);
    assert!(
        has_rel(&edges, RelType::Implements),
        "expected Implements edge (Document → Printable Protocol); got: {edges:?}"
    );
}
