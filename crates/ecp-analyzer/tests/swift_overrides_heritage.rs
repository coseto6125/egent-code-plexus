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

/// Stands for: a marked Swift override whose base method has no function
/// meta (the extractor produced none for it) gets no edge and an
/// `override-ambiguous` BlindSpot: the marker never makes an unconfirmed
/// branch `Exact` (codex ruling 2). Every Swift method kind the parser
/// emits gets a meta today, so the test removes the base's meta from the
/// parsed file; with it in place the same source binds `Exact`.
#[test]
fn test_overrides_swift_base_without_meta_records_ambiguous() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_AMBIGUOUS;
    use ecp_core::analyzer::provider::LanguageProvider;
    let src = "class SA {\n    func run(n: Int) {}\n}\nclass SB: SA {\n    override func run(n: Int) {}\n}\n";
    let parsed = ecp_analyzer::swift::parser::SwiftProvider::new()
        .unwrap()
        .parse_file(std::path::Path::new("S.swift"), src.as_bytes())
        .expect("parse");
    let with_meta = overrides_support::build(std::slice::from_ref(&parsed));
    let edges = overrides_support::overrides(&with_meta);
    assert_eq!(edges.len(), 1, "fixture premise: {edges:?}");
    assert_eq!((edges[0].0.line, edges[0].1.line), (4, 1));
    overrides_support::assert_evidence(&edges[0], Evidence::Exact);

    let mut stripped = parsed;
    let base_span = stripped
        .nodes
        .iter()
        .find(|n| n.name == "run" && n.span.0 == 1)
        .expect("SA.run")
        .span;
    let before = stripped.raw_function_metas.len();
    stripped.raw_function_metas.retain(|m| m.span != base_span);
    assert_eq!(
        stripped.raw_function_metas.len(),
        before - 1,
        "SA.run had a meta"
    );
    let graph = overrides_support::build(&[stripped]);
    let edges = overrides_support::overrides(&graph);
    assert!(edges.is_empty(), "{edges:?}");
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, _, row, hint) = &spots[0];
    assert_eq!((kind.as_str(), *row), (BS_KIND_OVERRIDE_AMBIGUOUS, 4));
    assert!(
        hint.contains("signature not comparable") && hint.contains("`SA`"),
        "{hint}"
    );
}
