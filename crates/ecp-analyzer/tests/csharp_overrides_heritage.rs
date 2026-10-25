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

/// Stands for: C# overloads by parameter type, so a base method whose
/// parameter type differs (`string` against `int`) is no target, and the
/// lookup goes on to the grandparent's matching `Run(int)` (codex ruling 2).
#[test]
fn test_overrides_csharp_parameter_type_differs_binds_grandparent() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[(
            "App/C.cs",
            "namespace App {\n    public class A {\n        public virtual void Run(int n) {}\n    }\n    public class B : A {\n        public virtual void Run(string s) {}\n    }\n    public class C : B {\n        public override void Run(int n) {}\n    }\n}\n",
        )],
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(8, 2)],
        "C.Run(int) -> A.Run(int), past B.Run(string)"
    );
    for edge in overrides_support::overrides(&graph) {
        overrides_support::assert_evidence(&edge, Evidence::Exact);
    }
    assert!(overrides_support::override_blind_spots(&graph).is_empty());
}

/// Stands for: a preprocessor line inside a base list (`#pragma warning
/// disable 618` around an obsolete base) is no base. It yields no
/// `override-base-unresolved` BlindSpot, and the real base still binds.
#[test]
fn test_overrides_csharp_pragma_in_base_list_is_no_base() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[(
            "App/C.cs",
            "namespace App {\n    public abstract class A {\n        public abstract void Run(int n);\n    }\n    public class C :\n#pragma warning disable 618\n        A\n#pragma warning restore 618\n    {\n        public override void Run(int n) {}\n    }\n}\n",
        )],
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(9, 2)],
        "C.Run -> A.Run"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}
