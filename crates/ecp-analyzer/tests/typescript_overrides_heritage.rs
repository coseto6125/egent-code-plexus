//! `Overrides` in TypeScript binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The parser captures no `override` modifier, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, graph_of, overrides, Evidence};

#[test]
fn test_overrides_typescript_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        ("a/decoy.ts", "export class Base {\n  run(): void {}\n}\n"),
        ("b/child.ts", "export class Base {\n  run(): void {}\n}\nexport class Child extends Base {\n  run(): void {}\n}\n"),
        "run",
        Evidence::Heuristic,
    );
}

#[test]
fn test_overrides_typescript_base_named_twice_emits_one_edge() {
    // `extends Base implements Base` reaches the one `Base.run` twice.
    let graph = graph_of(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[(
            "child.ts",
            "export class Base {\n  run(): void {}\n}\nexport class Child extends Base implements Base {\n  run(): void {}\n}\n",
        )],
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "one edge, not one per mention: {edges:?}");
    assert_eq!((edges[0].1.name.as_str(), edges[0].1.line), ("run", 1));
}

/// Stands for: an unmarked method binds the nearest declaration two levels
/// up, at the name-match confidence, never `Exact`.
#[test]
fn test_overrides_typescript_grandparent_emits_heuristic() {
    let graph = graph_of(
        &ecp_analyzer::typescript::TypeScriptProvider::new().unwrap(),
        &[(
            "c.ts",
            "export class A {\n  run(): void {}\n}\nexport class B extends A {}\nexport class C extends B {\n  run(): void {}\n}\n",
        )],
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "C.run -> A.run; got {edges:?}");
    assert_eq!((edges[0].0.line, edges[0].1.line), (5, 1), "{edges:?}");
    overrides_support::assert_evidence(&edges[0], Evidence::Heuristic);
    assert_eq!(
        edges[0].2,
        ecp_analyzer::post_process::overrides::NAME_MATCH_CONFIDENCE
    );
}
