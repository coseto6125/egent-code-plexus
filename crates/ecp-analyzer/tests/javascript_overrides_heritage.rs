//! `Overrides` in JavaScript binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! JavaScript has no override marker, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_javascript_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        ("a/decoy.js", "export class Base {\n  run() {}\n}\n"),
        ("b/child.js", "export class Base {\n  run() {}\n}\nexport class Child extends Base {\n  run() {}\n}\n"),
        "run",
        Evidence::Heuristic,
    );
}

/// Stands for: an external base is a reported gap at each method that may
/// override it, never a silent "no override".
#[test]
fn test_overrides_javascript_unresolved_base_reports_blindspot() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_BASE_UNRESOLVED;
    let graph = overrides_support::graph_of(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        &[("c.js", "export class C extends External {\n  run() {}\n}\n")],
    );
    let edges = overrides_support::overrides(&graph);
    assert!(edges.is_empty(), "{edges:?}");
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, file, row, hint) = &spots[0];
    assert_eq!(
        (kind.as_str(), file.as_str(), *row),
        (BS_KIND_OVERRIDE_BASE_UNRESOLVED, "c.js", 1)
    );
    assert!(
        hint.contains("`External`")
            && hint.contains("(0 same-named candidates)")
            && hint.contains("`run`"),
        "{hint}"
    );
}
