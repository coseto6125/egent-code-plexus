//! `Overrides` in C++ binds the declared base (slice P7): two types
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
fn test_overrides_cpp_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        ("a/decoy.hpp", "struct Base {\n    virtual void run() {}\n};\n"),
        ("b/child.cpp", "struct Base {\n    virtual void run() {}\n};\nstruct Child : public Base {\n    void run() override {}\n};\n"),
        "run",
        Evidence::Exact,
    );
}

/// Stands for: with several bases, each base keeps its own answer — C++
/// `D::run` overrides `A::run` and `B::run` both ([class.virtual]/2), so
/// the pass emits one edge per base and never only the first. Of two
/// overloads in a base, the one with the overrider's signature binds
/// (`E::stop`). A base method not declared virtual may hide rather than
/// override: that branch records `override-ambiguous`, and the virtual
/// branch still emits (`F::run`, codex ruling 2). The file is a `.h`,
/// which takes the C++ rules.
#[test]
fn test_overrides_cpp_multiple_bases_each_branch_answers() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_AMBIGUOUS;
    let graph = overrides_support::graph_of(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        &[(
            "shapes.h",
            "struct A {\n    virtual void run() {}\n};\nstruct B {\n    virtual void run() {}\n};\nstruct D : public A, public B {\n    void run() override {}\n};\nstruct O {\n    virtual void stop() {}\n    virtual void stop(int n) {}\n};\nstruct E : public O, public A {\n    void stop() override {}\n};\nstruct N {\n    void run() {}\n};\nstruct F : public N, public A {\n    void run() override {}\n};\n",
        )],
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(7, 1), (7, 4), (14, 10), (20, 1)],
        "D.run -> A.run and B.run; E.stop -> O.stop(); F.run -> A.run only"
    );
    for edge in overrides_support::overrides(&graph) {
        overrides_support::assert_evidence(&edge, Evidence::Exact);
    }
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, _, row, hint) = &spots[0];
    assert_eq!((kind.as_str(), *row), (BS_KIND_OVERRIDE_AMBIGUOUS, 20));
    assert!(
        hint.contains("`N`") && hint.contains("not declared virtual"),
        "{hint}"
    );
}
