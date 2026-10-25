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
            "namespace App {\n    public abstract class A {\n        public abstract void Run(int n);\n    }\n    public interface I {\n        void Stop();\n    }\n    public class C :\n#pragma warning disable 618\n        A,\n#pragma warning restore 618\n        I\n    {\n        public override void Run(int n) {}\n        public void Stop() {}\n    }\n}\n",
        )],
    );
    assert_eq!(
        overrides_support::heritage_confidences(&graph, "C").len(),
        2,
        "both real bases, A and I, stay heritage"
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(13, 2)],
        "C.Run -> A.Run"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: C# parameter types that differ only in a nullable annotation
/// (`string?` against `string`) may be one type (a reference type) or two
/// (`int?` is `Nullable<int>`). The lookup stops at that declaration with an
/// `override-ambiguous` BlindSpot: no edge, and no edge past it to the
/// grandparent's exact `Bind(string)` (codex P7 acceptance ruling, P-b).
#[test]
fn test_overrides_csharp_nullable_annotation_only_reports_ambiguous() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_AMBIGUOUS;
    let graph = overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[(
            "App/C.cs",
            "namespace App {\n    public abstract class A {\n        public virtual void Bind(string s) {}\n    }\n    public abstract class B : A {\n        public abstract void Bind(string? s);\n    }\n    public class C : B {\n        public override void Bind(string s) {}\n    }\n}\n",
        )],
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [],
        "C.Bind stops at B.Bind(string?): no edge, and none past B to A.Bind(string)"
    );
    let spots = overrides_support::override_blind_spots(&graph);
    let mine: Vec<_> = spots.iter().filter(|s| s.2 == 8).collect();
    assert_eq!(mine.len(), 1, "{spots:?}");
    assert_eq!(mine[0].0, BS_KIND_OVERRIDE_AMBIGUOUS, "{spots:?}");
    assert!(mine[0].3.contains("nullable"), "{}", mine[0].3);
}

fn csharp_graph(source: &str) -> ecp_core::graph::ZeroCopyGraph {
    overrides_support::graph_of(
        &ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap(),
        &[("App/Conv.cs", source)],
    )
}

/// `(source row, target row)` of every `HasMethod` edge, sorted.
fn has_method_rows(graph: &ecp_core::graph::ZeroCopyGraph) -> Vec<(u32, u32)> {
    let mut rows: Vec<(u32, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == ecp_core::graph::RelType::HasMethod)
        .map(|e| {
            (
                graph.nodes[e.source as usize].span.0,
                graph.nodes[e.target as usize].span.0,
            )
        })
        .collect();
    rows.sort_unstable();
    rows
}

/// `(0-based row, name)` of every `Class` node, sorted.
fn class_rows(graph: &ecp_core::graph::ZeroCopyGraph) -> Vec<(u32, String)> {
    let pool = graph.string_pool.as_slice();
    let mut rows: Vec<(u32, String)> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == ecp_core::graph::NodeKind::Class)
        .map(|n| (n.span.0, n.name.resolve(pool).to_string()))
        .collect();
    rows.sort_unstable();
    rows
}

/// Stands for: C# `Conv` and `Conv<T>` in one file are two types (codex
/// ruling P-a, Newtonsoft `JsonConverter` / `JsonConverter<T>`). `Conv<T>`
/// keeps its name and owns its own `CanConvert`, so `Plain.CanConvert` finds
/// one `CanConvert` in `Conv`, and `Conv<T>.CanConvert` overrides it. Before
/// the fix `Conv<T>` was a nameless tombstone, its method hung under `Conv`,
/// and no `Overrides` edge formed (`override-ambiguous`).
#[test]
fn test_overrides_csharp_generic_beside_plain_binds_plain_base_method() {
    let graph = csharp_graph(
        "namespace N\n{\n    public abstract class Conv { public abstract bool CanConvert(int t); }\n    public abstract class Conv<T> : Conv { public sealed override bool CanConvert(int t) { return true; } }\n    public class Plain : Conv { public override bool CanConvert(int t) { return false; } }\n    public class Gen<T> : Conv { public override bool CanConvert(int t) { return false; } }\n}\n",
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(3, 2), (4, 2), (5, 2)],
        "Conv<T>, Plain and Gen<T> each override Conv.CanConvert"
    );
    assert_eq!(
        class_rows(&graph),
        [
            (2, "Conv".to_string()),
            (3, "Conv".to_string()),
            (4, "Plain".to_string()),
            (5, "Gen".to_string()),
        ],
        "Conv<T> keeps its name"
    );
    assert!(
        has_method_rows(&graph).contains(&(3, 3)),
        "Conv<T> owns its own CanConvert: {:?}",
        has_method_rows(&graph)
    );
    assert!(
        !has_method_rows(&graph).contains(&(2, 3)),
        "Conv does not own Conv<T>'s CanConvert: {:?}",
        has_method_rows(&graph)
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: a base spelled with type arguments names the type of that
/// arity. `IntConv : Conv<int>` extends `Conv<T>`, not `Conv`, so its
/// `Read` overrides the `Read` that only `Conv<T>` declares. Before the fix
/// `Conv<int>` resolved to no project type.
#[test]
fn test_overrides_csharp_generic_base_spelling_binds_generic_type_method() {
    let graph = csharp_graph(
        "namespace N\n{\n    public abstract class Conv { public abstract bool CanConvert(int t); }\n    public abstract class Conv<T> : Conv { public abstract T Read(); }\n    public class IntConv : Conv<int> { public override int Read() { return 1; } }\n}\n",
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(4, 3)],
        "IntConv.Read -> Conv<T>.Read"
    );
    assert_eq!(
        overrides_support::heritage_confidences(&graph, "IntConv").len(),
        1,
        "Conv<int> is one heritage edge"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: the declaration order does not matter. With `Conv<T>`
/// first, `: Conv` still names the plain `Conv`, and both overrides bind it.
#[test]
fn test_overrides_csharp_generic_declared_first_binds_plain_base_method() {
    let graph = csharp_graph(
        "namespace N\n{\n    public abstract class Conv<T> : Conv { public sealed override bool CanConvert(int t) { return true; } }\n    public abstract class Conv { public abstract bool CanConvert(int t); }\n    public class Plain : Conv { public override bool CanConvert(int t) { return false; } }\n}\n",
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(2, 3), (4, 3)],
        "Conv<T>.CanConvert and Plain.CanConvert -> Conv.CanConvert"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: a nested pair `Outer.Conv` / `Outer.Conv<T>` splits the same
/// way, and a nested subclass binds the plain one.
#[test]
fn test_overrides_csharp_nested_generic_beside_plain_binds_plain_base_method() {
    let graph = csharp_graph(
        "namespace N\n{\n    public class Outer\n    {\n        public abstract class Conv { public abstract bool CanConvert(int t); }\n        public abstract class Conv<T> : Conv { public sealed override bool CanConvert(int t) { return true; } }\n        public class Plain : Conv { public override bool CanConvert(int t) { return false; } }\n    }\n}\n",
    );
    assert_eq!(
        overrides_support::override_rows(&graph),
        [(5, 4), (6, 4)],
        "Outer.Conv<T>.CanConvert and Outer.Plain.CanConvert -> Outer.Conv.CanConvert"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: only a different arity splits. Two same-named types of one
/// arity in one file stay a uid collision: the second is the nameless
/// tombstone it was before the fix. Passes before the fix as well; it
/// guards the restriction of codex ruling P-a.
#[test]
fn test_overrides_csharp_same_arity_twins_second_tombstoned() {
    let graph = csharp_graph(
        "namespace N\n{\n    public class Conv<T> { public virtual bool CanConvert(int t) { return true; } }\n    public class Conv<U> { public virtual bool CanConvert(int t) { return false; } }\n}\n",
    );
    assert_eq!(
        class_rows(&graph),
        [(2, "Conv".to_string()), (3, String::new())],
        "the second Conv of arity 1 is a tombstone"
    );
}
