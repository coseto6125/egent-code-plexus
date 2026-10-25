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
