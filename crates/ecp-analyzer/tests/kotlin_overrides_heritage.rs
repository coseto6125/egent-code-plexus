//! `Overrides` in Kotlin binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! `override` marks the method.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_kotlin_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        ("a/Base.kt", "package a\nopen class Base {\n    open fun run() {}\n}\n"),
        ("b/Child.kt", "package b\nopen class Base {\n    open fun run() {}\n}\nclass Child : Base() {\n    override fun run() {}\n}\n"),
        "run",
        Evidence::Exact,
    );
}

/// Stands for: a marked override binds the nearest declaration two levels
/// up when the middle base lacks it, at `Exact` 1.0.
#[test]
fn test_overrides_kotlin_grandparent_resolves_target() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        &[(
            "C.kt",
            "open class A {\n    open fun run() {}\n}\nopen class B : A() {\n}\nclass C : B() {\n    override fun run() {}\n}\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 1, "C.run -> A.run; got {edges:?}");
    assert_eq!((edges[0].0.line, edges[0].1.line), (6, 1), "{edges:?}");
    overrides_support::assert_evidence(&edges[0], Evidence::Exact);
    assert_eq!(edges[0].2, 1.0);
}

/// Stands for: a marked Kotlin override whose signature the meta cannot
/// compare (the extractor leaves a function-type parameter's type blank)
/// gets no edge and an `override-ambiguous` BlindSpot, never a guessed
/// `Exact` edge; a comparable sibling still binds `Exact` (codex ruling 2).
#[test]
fn test_overrides_kotlin_signature_not_comparable_records_ambiguous() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_AMBIGUOUS;
    let graph = overrides_support::graph_of(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        &[(
            "K.kt",
            "open class KA {\n    open fun run(cb: () -> Unit) {}\n    open fun walk(n: Int) {}\n}\nclass KB : KA() {\n    override fun run(cb: () -> Unit) {}\n    override fun walk(n: Int) {}\n}\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 1, "KB.walk -> KA.walk only; got {edges:?}");
    assert_eq!(edges[0].0.name, "walk");
    assert_eq!((edges[0].0.line, edges[0].1.line), (6, 2), "{edges:?}");
    overrides_support::assert_evidence(&edges[0], Evidence::Exact);
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, _, row, hint) = &spots[0];
    assert_eq!((kind.as_str(), *row), (BS_KIND_OVERRIDE_AMBIGUOUS, 5));
    assert!(
        hint.contains("signature not comparable") && hint.contains("`KA`"),
        "{hint}"
    );
}
