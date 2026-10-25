//! `Overrides` in Python binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The method carries no `@override`, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_python_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::python::PythonProvider::new().unwrap(),
        ("a/decoy.py", "class Base:\n    def run(self):\n        pass\n"),
        ("b/child.py", "class Base:\n    def run(self):\n        pass\n\n\nclass Child(Base):\n    def run(self):\n        pass\n"),
        "run",
        Evidence::Heuristic,
    );
}

fn python() -> ecp_analyzer::python::PythonProvider {
    ecp_analyzer::python::PythonProvider::new().unwrap()
}

/// Stands for: Python binds the first class of the C3 method resolution
/// order that declares the name, never the first hit of a depth-first or
/// breadth-first walk.
#[test]
fn test_overrides_python_diamond_respects_mro() {
    // MRO(D) = D, B, C, A: C.m comes before A.m.
    let graph = overrides_support::graph_of(
        &python(),
        &[(
            "d.py",
            "class A:\n    def m(self):\n        pass\n\n\nclass B(A):\n    pass\n\n\nclass C(A):\n    def m(self):\n        pass\n\n\nclass D(B, C):\n    def m(self):\n        pass\n",
        )],
    );
    let rows = overrides_support::override_rows(&graph);
    assert_eq!(
        rows,
        [(10, 1), (15, 10)],
        "C.m -> A.m and D.m -> C.m; never D.m -> A.m"
    );
    for edge in overrides_support::overrides(&graph) {
        overrides_support::assert_evidence(&edge, Evidence::Heuristic);
    }
}

/// Stands for: an unresolved base listed before a resolved one sits earlier
/// in the MRO, so it ends the lookup with a BlindSpot; the farther base is
/// never reached past it. Listed after, it changes nothing.
#[test]
fn test_overrides_python_unresolved_base_first_blocks_later_base() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_BASE_UNRESOLVED;
    let source = |bases: &str| {
        format!("class B:\n    def run(self):\n        pass\n\n\nclass C({bases}):\n    def run(self):\n        pass\n")
    };
    let blocked =
        overrides_support::graph_of(&python(), &[("c.py", source("External, B").as_str())]);
    assert!(overrides_support::overrides(&blocked).is_empty());
    let spots = overrides_support::override_blind_spots(&blocked);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, _, row, hint) = &spots[0];
    assert_eq!((kind.as_str(), *row), (BS_KIND_OVERRIDE_BASE_UNRESOLVED, 6));
    assert!(
        hint.contains("`External`") && hint.contains("`C`"),
        "{hint}"
    );

    let reached =
        overrides_support::graph_of(&python(), &[("c.py", source("B, External").as_str())]);
    assert_eq!(overrides_support::override_rows(&reached), [(6, 1)]);
    assert!(overrides_support::override_blind_spots(&reached).is_empty());
}

/// Stands for: the chosen Python rule (codex ruling 2). The meta keeps
/// neither a default value nor the `*` of a splat, so a parameter count
/// that differs pairs by name alone: a `Heuristic` edge, even under
/// `@override`. An equal count confirms the pair, and `@override` makes it
/// `Exact`.
#[test]
fn test_overrides_python_default_arg_arity_difference_emits_heuristic() {
    let graph = overrides_support::graph_of(
        &python(),
        &[(
            "e.py",
            "from typing import override\n\n\nclass A:\n    def run(self, x=1):\n        pass\n\n\nclass B(A):\n    @override\n    def run(self):\n        pass\n\n\nclass C(A):\n    @override\n    def run(self, x=2):\n        pass\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 2, "B.run -> A.run, C.run -> A.run: {edges:?}");
    for edge in &edges {
        assert_eq!(edge.1.line, 4, "both bind A.run: {edges:?}");
        let expected = if (9..=10).contains(&edge.0.line) {
            Evidence::Heuristic
        } else {
            assert!((15..=16).contains(&edge.0.line), "{edges:?}");
            Evidence::Exact
        };
        overrides_support::assert_evidence(edge, expected);
    }
    assert!(overrides_support::override_blind_spots(&graph).is_empty());
}

/// Stands for: an edge carries the weakest resolver confidence on the path
/// it walked (codex ruling 2). `C` imports `B` (0.95); `B` names `A` by a
/// bare name (the `Global` tier, 0.7). `C.run -> A.run` is a 0.6
/// `base-by-name` edge even under `@override`, never the first hop's 0.95.
#[test]
fn test_overrides_python_path_minimum_confidence_binds_base_by_name() {
    let graph = overrides_support::graph_of(
        &python(),
        &[
            ("py/a.py", "class A:\n    def run(self):\n        pass\n"),
            ("py/b.py", "class B(A):\n    pass\n"),
            (
                "py/c.py",
                "from typing import override\nfrom b import B\n\n\nclass C(B):\n    @override\n    def run(self):\n        pass\n",
            ),
        ],
    );
    assert_eq!(
        (
            overrides_support::heritage_confidences(&graph, "C"),
            overrides_support::heritage_confidences(&graph, "B"),
        ),
        (vec![0.95], vec![0.7]),
        "fixture premise: an imported hop, then a bare-name hop"
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 1, "C.run -> A.run: {edges:?}");
    let (source, target, confidence, reason) = &edges[0];
    assert_eq!(
        (source.file.as_str(), target.file.as_str()),
        ("py/c.py", "py/a.py")
    );
    assert_eq!(
        *confidence,
        ecp_analyzer::post_process::overrides::NAME_MATCH_CONFIDENCE
    );
    assert_eq!(reason, "post_process:overrides:base-by-name");
}
