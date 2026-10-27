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

// ── C# partial types (codex ruling P-d) ─────────────────────────────────

/// One partial type, two files: `Writer.cs` declares `Flush`,
/// `Writer.Async.cs` declares `FlushAsync` (Newtonsoft `JsonWriter.cs` /
/// `JsonWriter.Async.cs`).
const WRITER: &str = "namespace Lib\n{\n    public abstract partial class Writer\n    {\n        public virtual void Flush() {}\n    }\n}\n";
const WRITER_ASYNC: &str = "namespace Lib\n{\n    public abstract partial class Writer\n    {\n        public virtual void FlushAsync() {}\n    }\n}\n";
/// A subclass that overrides one method of each part (rows 4 and 5).
const TEXT_WRITER: &str = "namespace Lib\n{\n    public class TextWriter : Writer\n    {\n        public override void Flush() {}\n        public override void FlushAsync() {}\n    }\n}\n";

fn csharp() -> ecp_analyzer::c_sharp::parser::CSharpProvider {
    ecp_analyzer::c_sharp::parser::CSharpProvider::new().unwrap()
}

/// `((file, name, row), (file, name, row))` of every `Overrides` edge,
/// sorted: the source and the target a reader of `ecp impact` sees.
type EndKey = (String, String, u32);
fn override_ends(graph: &ecp_core::graph::ZeroCopyGraph) -> Vec<(EndKey, EndKey)> {
    let key = |end: &overrides_support::End| (end.file.clone(), end.name.clone(), end.line);
    let mut ends: Vec<(EndKey, EndKey)> = overrides_support::overrides(graph)
        .iter()
        .map(|(source, target, _, _)| (key(source), key(target)))
        .collect();
    ends.sort();
    ends
}

fn end_key(file: &str, name: &str, row: u32) -> EndKey {
    (file.to_string(), name.to_string(), row)
}

/// Asserts the BlindSpot of a base that names several unmerged types: no
/// `Overrides` edge, and one `override-base-ambiguous` BlindSpot per
/// overriding method of `TEXT_WRITER` in `child`, with the candidate count.
fn assert_base_ambiguous(graph: &ecp_core::graph::ZeroCopyGraph, child: &str) {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_BASE_AMBIGUOUS;
    assert_eq!(override_ends(graph), [], "no edge to either Writer");
    let mut spots: Vec<(String, String, u32)> = overrides_support::override_blind_spots(graph)
        .into_iter()
        .map(|(kind, file, row, hint)| {
            assert!(hint.contains("none of 2 candidate types"), "{hint}");
            (kind, file, row)
        })
        .collect();
    spots.sort();
    assert_eq!(
        spots,
        [
            (
                BS_KIND_OVERRIDE_BASE_AMBIGUOUS.to_string(),
                child.to_string(),
                4
            ),
            (
                BS_KIND_OVERRIDE_BASE_AMBIGUOUS.to_string(),
                child.to_string(),
                5
            ),
        ]
    );
}

/// Stands for: the parts of one partial type in one project are one type.
/// `TextWriter : Writer` binds it, and each override ends at the method in
/// the part that declares it. Before the fix the base named two types, so
/// both methods got `override-base-ambiguous` and no edge (326 Newtonsoft
/// pairs).
#[test]
fn test_overrides_csharp_partial_parts_one_project_bind_declaring_part() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[
            ("Lib/Writer.cs", WRITER),
            ("Lib/Writer.Async.cs", WRITER_ASYNC),
            ("Lib/TextWriter.cs", TEXT_WRITER),
        ],
        &["Lib"],
    );
    assert_eq!(
        override_ends(&graph),
        [
            (
                end_key("Lib/TextWriter.cs", "Flush", 4),
                end_key("Lib/Writer.cs", "Flush", 4)
            ),
            (
                end_key("Lib/TextWriter.cs", "FlushAsync", 5),
                end_key("Lib/Writer.Async.cs", "FlushAsync", 4)
            ),
        ]
    );
    assert_eq!(
        overrides_support::heritage_confidences(&graph, "TextWriter").len(),
        1,
        "Writer is one Extends edge"
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: the bases one part lists are the type's. `partial class A :
/// Base` and `partial class A` with the override: the override in the
/// second part reaches `Base.Run`, with the evidence of the base the first
/// part bound. (C# overrides only a class member, so the base is an
/// abstract class, not an interface.) Before the fix the second part had no
/// bases, and its method got no edge.
#[test]
fn test_overrides_csharp_partial_heritage_on_one_part_binds_from_other_part() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[
            (
                "Lib/A.cs",
                "namespace Lib\n{\n    public abstract class Base\n    {\n        public abstract void Run();\n    }\n    public partial class A : Base\n    {\n    }\n}\n",
            ),
            (
                "Lib/A.Run.cs",
                "namespace Lib\n{\n    public partial class A\n    {\n        public override void Run() {}\n    }\n}\n",
            ),
        ],
        &["Lib"],
    );
    assert_eq!(
        override_ends(&graph),
        [(
            end_key("Lib/A.Run.cs", "Run", 4),
            end_key("Lib/A.cs", "Run", 4)
        )]
    );
    overrides_support::assert_evidence(&overrides_support::overrides(&graph)[0], Evidence::Exact);
    assert!(overrides_support::override_blind_spots(&graph).is_empty());
}

/// Stands for: the bases a same-file part lists are the type's too. The
/// second `partial class A : Base` of one file is a uid twin of the first,
/// so it has no node of its own; the override in the first part still
/// reaches `Base.Run`. Before the fix Pass 1.9 skipped the twin as a
/// tombstone: `A` had no supertype and `Run` no edge.
#[test]
fn test_overrides_csharp_same_file_partial_twin_base_list_binds() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[(
            "Lib/A.cs",
            "namespace Lib\n{\n    public abstract class Base\n    {\n        public abstract void Run();\n    }\n    public partial class A\n    {\n        public override void Run() {}\n    }\n    public partial class A : Base\n    {\n    }\n}\n",
        )],
        &["Lib"],
    );
    assert_eq!(
        override_ends(&graph),
        [(end_key("Lib/A.cs", "Run", 8), end_key("Lib/A.cs", "Run", 4))]
    );
    overrides_support::assert_evidence(&overrides_support::overrides(&graph)[0], Evidence::Exact);
    assert!(overrides_support::override_blind_spots(&graph).is_empty());
}

/// Stands for: parts in two projects are two types (an unknown or another
/// compilation is never a shared one). The base stays ambiguous, as before
/// the fix. Guards the project field of the merge key; passes before the
/// fix as well.
#[test]
fn test_overrides_csharp_partial_parts_two_projects_report_blindspot() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[
            ("P1/Writer.cs", WRITER),
            ("P2/Writer.Async.cs", WRITER_ASYNC),
            ("P1/TextWriter.cs", TEXT_WRITER),
        ],
        &["P1", "P2"],
    );
    assert_base_ambiguous(&graph, "P1/TextWriter.cs");
}

/// Stands for: parts whose project the index does not know never merge.
/// The fixture of the one-project test with no project: the base stays
/// ambiguous. Guards the unknown-project rule; passes before the fix as
/// well.
#[test]
fn test_overrides_csharp_partial_parts_unknown_project_report_blindspot() {
    let graph = overrides_support::graph_of(
        &csharp(),
        &[
            ("Lib/Writer.cs", WRITER),
            ("Lib/Writer.Async.cs", WRITER_ASYNC),
            ("Lib/TextWriter.cs", TEXT_WRITER),
        ],
    );
    assert_base_ambiguous(&graph, "Lib/TextWriter.cs");
}

/// Stands for: the generic arity is part of the merge key. `partial class
/// X` in two files is one type, `partial class X<T>` another: `Y : X`
/// binds the first, its overrides reach both of its parts, and none reaches
/// `X<T>.Run`. Before the fix `X` named three types and bound none.
#[test]
fn test_overrides_csharp_partial_and_generic_partial_do_not_merge() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[
            (
                "Lib/X.cs",
                "namespace Lib\n{\n    public partial class X\n    {\n        public virtual void Run() {}\n    }\n}\n",
            ),
            (
                "Lib/X.More.cs",
                "namespace Lib\n{\n    public partial class X\n    {\n        public virtual void Stop() {}\n    }\n}\n",
            ),
            (
                "Lib/XT.cs",
                "namespace Lib\n{\n    public partial class X<T>\n    {\n        public virtual void Run() {}\n    }\n}\n",
            ),
            (
                "Lib/Y.cs",
                "namespace Lib\n{\n    public class Y : X\n    {\n        public override void Run() {}\n        public override void Stop() {}\n    }\n}\n",
            ),
        ],
        &["Lib"],
    );
    assert_eq!(
        override_ends(&graph),
        [
            (end_key("Lib/Y.cs", "Run", 4), end_key("Lib/X.cs", "Run", 4)),
            (
                end_key("Lib/Y.cs", "Stop", 5),
                end_key("Lib/X.More.cs", "Stop", 4)
            ),
        ]
    );
    assert!(
        overrides_support::override_blind_spots(&graph).is_empty(),
        "{:?}",
        overrides_support::override_blind_spots(&graph)
    );
}

/// Stands for: only declarations that all carry `partial` merge. Two plain
/// `class Writer`, and a `partial class Writer` beside a plain one, stay two
/// types, and the base stays ambiguous. Guards the partial-modifier rule;
/// passes before the fix as well.
#[test]
fn test_overrides_csharp_non_partial_same_name_pair_report_blindspot() {
    let plain = WRITER.replace("abstract partial class", "abstract class");
    let plain_async = WRITER_ASYNC.replace("abstract partial class", "abstract class");
    for (first, second) in [
        (plain.as_str(), plain_async.as_str()),
        (WRITER, plain_async.as_str()),
    ] {
        let graph = overrides_support::graph_of_projects(
            &csharp(),
            &[
                ("Lib/Writer.cs", first),
                ("Lib/Writer.Async.cs", second),
                ("Lib/TextWriter.cs", TEXT_WRITER),
            ],
            &["Lib"],
        );
        assert_base_ambiguous(&graph, "Lib/TextWriter.cs");
    }
}

/// Stands for: Pass 2 reads the parts as one type. In `Writer.cs`, both
/// `this.WriteAsync()` and a bare `WriteAsync()` bind the `WriteAsync` that
/// `Writer.Async.cs` declares, never `Other.WriteAsync`. Before the fix the
/// ladder found the member outside the caller's file under a type name two
/// types share (ambiguous), and the bare name had two global candidates.
#[test]
fn test_resolve_call_csharp_partial_part_calls_other_part_binds_it() {
    let graph = overrides_support::graph_of_projects(
        &csharp(),
        &[
            (
                "Lib/Writer.cs",
                "namespace Lib\n{\n    public partial class Writer\n    {\n        public void ViaThis() { this.WriteAsync(); }\n        public void Bare() { WriteAsync(); }\n    }\n}\n",
            ),
            (
                "Lib/Writer.Async.cs",
                "namespace Lib\n{\n    public partial class Writer\n    {\n        public void WriteAsync() {}\n    }\n}\n",
            ),
            (
                "Lib/Other.cs",
                "namespace Lib\n{\n    public class Other\n    {\n        public void WriteAsync() {}\n    }\n}\n",
            ),
        ],
        &["Lib"],
    );
    let mut calls: Vec<(String, String, String)> =
        overrides_support::edge_ends(&graph, ecp_core::graph::RelType::Calls)
            .into_iter()
            .filter(|(source, _)| source.file == "Lib/Writer.cs")
            .map(|(source, target)| (source.name, target.file, target.name))
            .collect();
    calls.sort();
    let expected = |caller: &str| {
        (
            caller.to_string(),
            "Lib/Writer.Async.cs".to_string(),
            "WriteAsync".to_string(),
        )
    };
    assert_eq!(calls, [expected("Bare"), expected("ViaThis")]);
}

/// The partial marker of every type in `source`: `(name, declaring scope)`
/// for each `partial` type, `None` for the others.
fn partial_scopes(source: &str) -> Vec<(String, Option<String>)> {
    use ecp_core::analyzer::provider::LanguageProvider;
    csharp()
        .parse_file(std::path::Path::new("Lib/S.cs"), source.as_bytes())
        .unwrap()
        .nodes
        .iter()
        .filter(|n| n.kind.is_type())
        .map(|n| (n.name.clone(), n.partial_scope().map(str::to_string)))
        .collect()
}

/// Stands for: the declaring scope in the merge key is the namespace (a
/// file-scoped one, or block namespaces through a preprocessor block) and
/// each enclosing type with its generic arity. A type without `partial`
/// carries no marker.
#[test]
fn test_parse_file_csharp_partial_type_records_declaring_scope() {
    assert_eq!(
        partial_scopes(
            "namespace Lib.Json;\npublic partial class Outer<T>\n{\n    public partial class Inner {}\n}\npublic class Plain {}\n"
        ),
        [
            ("Outer".to_string(), Some("Lib.Json".to_string())),
            ("Inner".to_string(), Some("Lib.Json+Outer`1".to_string())),
            ("Plain".to_string(), None),
        ]
    );
    assert_eq!(
        partial_scopes(
            "#if HAVE_ASYNC\nnamespace Lib\n{\n    namespace Json\n    {\n        public partial struct W {}\n    }\n}\n#endif\n"
        ),
        [("W".to_string(), Some("Lib.Json".to_string()))]
    );
}
