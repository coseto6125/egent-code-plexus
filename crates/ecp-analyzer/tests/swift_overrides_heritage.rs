//! `Overrides` in Swift binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The parser records Swift's mandatory `override` modifier as the marker,
//! so the pair is `Exact` (it was a name match until the codex ruling on P7
//! added the capture).

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_swift_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        ("a/Decoy.swift", "class Base {\n    func run() {}\n}\n"),
        ("b/Child.swift", "class Base {\n    func run() {}\n}\nclass Child: Base {\n    override func run() {}\n}\n"),
        "run",
        Evidence::Exact,
    );
}

/// Stands for: Swift's mandatory `override` marks the method, so a
/// grandparent binding is `Exact` 1.0; an unmarked protocol requirement
/// stays a name match.
#[test]
fn test_overrides_swift_marked_grandparent_emits_exact() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::swift::parser::SwiftProvider::new().unwrap(),
        &[(
            "Shapes.swift",
            "class A {\n    func run() {}\n}\nclass B: A {\n}\nclass C: B {\n    override func run() {}\n}\nprotocol P {\n    func walk()\n}\nclass D: P {\n    func walk() {}\n}\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(
        edges.len(),
        2,
        "C.run -> A.run, D.walk -> P.walk: {edges:?}"
    );
    let marked = edges.iter().find(|e| e.0.name == "run").expect("run edge");
    assert_eq!((marked.0.line, marked.1.line), (6, 1), "{edges:?}");
    overrides_support::assert_evidence(marked, Evidence::Exact);
    assert_eq!(marked.2, 1.0);
    let unmarked = edges
        .iter()
        .find(|e| e.0.name == "walk")
        .expect("walk edge");
    assert_eq!((unmarked.0.line, unmarked.1.line), (12, 9), "{edges:?}");
    overrides_support::assert_evidence(unmarked, Evidence::Heuristic);
}
