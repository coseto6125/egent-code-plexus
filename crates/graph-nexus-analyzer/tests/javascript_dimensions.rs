//! Parity regression tests for four previously-reported JavaScript matrix gaps:
//! Heritage / Imports / Constructor / EntryPoint.
//!
//! ## Per-dimension classification
//!
//! **Heritage (Extends/Implements edges)**
//! Classification: fixture/corpus gap.
//! The JS parser already captures `@heritage` from `class_heritage` nodes and
//! populates `RawNode.heritage`; the graph builder converts that into
//! Extends/Implements edges. The verifier returns 0 because the JS sample
//! corpus (Express.js) has zero class declarations — it uses CommonJS
//! `require()` and prototype-based patterns with no `class … extends …` syntax.
//! No code change needed; the parser emits correctly when given proper input
//! (verified by `heritage_extends_populates` below).
//!
//! **Imports edges**
//! Classification: fixture/corpus gap.
//! The JS parser already captures `import_statement` patterns and emits
//! `RawImport` structs; the graph builder turns those into Imports edges.
//! The verifier returns 0 because the Express.js corpus uses only CommonJS
//! `require()` (no ES `import` statements). No code change needed; the parser
//! emits correctly when given proper input (verified by `imports_es_module`
//! below).
//!
//! **Constructor nodes**
//! Classification: real parser bug — fixed in this change.
//! JS has no separate `constructor_declaration` grammar node. Constructors are
//! `method_definition` nodes whose `property_identifier` name is literally
//! `"constructor"`. The parser was emitting them as `NodeKind::Method`.
//! Fix: promote `Method` → `Constructor` in `parser.rs` when `name == "constructor"`.
//!
//! **EntryPoint nodes**
//! Classification: design-correct, README wrong — deferred to E.5 README downgrade.
//! `entry_points.rs` explicitly documents that TS/JS/Python/PHP/Ruby have no
//! language-level main convention and are already covered via routes/scripts.
//! The README cell for JS Entry should drop from ✓ to ☐ in E.5.

use graph_nexus_analyzer::javascript::parser::JavaScriptProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = JavaScriptProvider::new().expect("provider");
    p.parse_file(Path::new("test.js"), src.as_bytes())
        .expect("parse")
}

fn has_node(graph: &LocalGraph, name: &str, kind: NodeKind) -> bool {
    graph.nodes.iter().any(|n| n.name == name && n.kind == kind)
}

// ── Constructor ──────────────────────────────────────────────────────────────

/// JS constructor is a `method_definition` named `"constructor"`.
/// Parser must promote it to `NodeKind::Constructor`, not leave it as Method.
#[test]
fn constructor_emits_as_constructor_not_method() {
    let src = r#"
class Animal {
    constructor(name) { this.name = name; }
    speak() {}
}
"#;
    let g = parse(src);
    assert!(
        has_node(&g, "constructor", NodeKind::Constructor),
        "constructor must emit as Constructor; nodes: {:#?}",
        g.nodes
    );
    assert!(
        !has_node(&g, "constructor", NodeKind::Method),
        "constructor must NOT appear as Method; nodes: {:#?}",
        g.nodes
    );
}

/// Subclass constructor is also promoted correctly.
#[test]
fn constructor_in_subclass_emits() {
    let src = r#"
import { foo } from './utils.js';
class Animal { constructor(name) { this.name = name; } }
class Cat extends Animal { constructor(name) { super(name); } meow() {} }
"#;
    let g = parse(src);
    let ctor_count = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .count();
    assert_eq!(
        ctor_count, 2,
        "two constructors expected; nodes: {:#?}",
        g.nodes
    );
}

/// Non-constructor methods must remain as Method.
#[test]
fn non_constructor_method_stays_method() {
    let src = "class Foo { bar() {} baz() {} }";
    let g = parse(src);
    assert!(
        has_node(&g, "bar", NodeKind::Method),
        "regular method must stay as Method"
    );
    assert!(
        has_node(&g, "baz", NodeKind::Method),
        "regular method must stay as Method"
    );
    assert!(
        !g.nodes.iter().any(|n| n.kind == NodeKind::Constructor),
        "no constructor nodes expected in class with no constructor method"
    );
}

// ── Heritage (corpus-gap verification) ──────────────────────────────────────

/// Verify the parser populates `RawNode.heritage` for `class X extends Y`.
/// Heritage → Extends/Implements edges are emitted by the graph builder from
/// this field; verifying the field is sufficient to confirm no parser bug.
#[test]
fn heritage_extends_populates() {
    let src = "class Cat extends Animal { meow() {} }";
    let g = parse(src);
    let cat = g
        .nodes
        .iter()
        .find(|n| n.name == "Cat" && n.kind == NodeKind::Class)
        .expect("Cat class node must exist");
    assert!(
        cat.heritage.contains(&"Animal".to_string()),
        "Cat.heritage must contain \"Animal\"; got: {:?}",
        cat.heritage
    );
}

// ── Imports (corpus-gap verification) ────────────────────────────────────────

/// Verify the parser emits RawImports for ES `import` statements.
/// These become Imports edges in the graph builder; verifying RawImports
/// confirms no parser bug.
#[test]
fn imports_es_module_emits() {
    let src = r#"import { foo } from './utils.js';"#;
    let g = parse(src);
    assert!(
        !g.imports.is_empty(),
        "ES import must produce RawImport entries; imports: {:#?}",
        g.imports
    );
    assert!(
        g.imports.iter().any(|i| i.source == "./utils.js"),
        "import source must match; imports: {:#?}",
        g.imports
    );
}
