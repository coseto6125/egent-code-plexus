//! `Overrides` in C# binds the declared base (slice P7): two types
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
fn test_overrides_csharp_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        ("A/Base.cs", "namespace A {\n    public class Base {\n        public virtual void Run() {}\n    }\n}\n"),
        ("B/Child.cs", "namespace B {\n    public class Base {\n        public virtual void Run() {}\n    }\n    public class Child : Base {\n        public override void Run() {}\n    }\n}\n"),
        "Run",
        Evidence::Exact,
    );
}

/// Stands for: a declared base that names several project types (the
/// stand-in for C# partial parts and duplicated source trees) binds none of
/// them: no edge to the first candidate, one `override-base-ambiguous`
/// BlindSpot with the candidate count.
#[test]
fn test_overrides_csharp_ambiguous_base_reports_blindspot() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_BASE_AMBIGUOUS;
    let copy = "namespace Lib {\n    public class Base {\n        public virtual void Run() {}\n    }\n}\n";
    let graph = overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[
            ("guava/Base.cs", copy),
            ("android/guava/Base.cs", copy),
            (
                "app/Child.cs",
                "using Lib;\nnamespace App {\n    public class Child : Base {\n        public override void Run() {}\n    }\n}\n",
            ),
        ],
    );
    let edges = overrides_support::overrides(&graph);
    assert!(edges.is_empty(), "no edge to either copy: {edges:?}");
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, file, row, hint) = &spots[0];
    assert_eq!(
        (kind.as_str(), file.as_str(), *row),
        (BS_KIND_OVERRIDE_BASE_AMBIGUOUS, "app/Child.cs", 3),
        "{spots:?}"
    );
    assert!(
        hint.contains("`Base`") && hint.contains("2 candidate types"),
        "{hint}"
    );
}
