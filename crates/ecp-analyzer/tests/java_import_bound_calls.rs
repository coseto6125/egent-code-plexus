//! A Java import that names a repo file binds the callee to that file's
//! member (ImportScoped); one that names nothing local leaves no edge; any
//! other import keeps the resolution path the call had before the import tier.
mod instantiation_calls_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{assert_call_targets, calls_from, graph_of};
use std::path::Path;

const HELPER: (&str, &str) = (
    "src/main/java/pkg/Helper.java",
    "package pkg; public class Helper { public static void helper() {} }",
);

fn java_graph(files: &[(&str, &str)]) -> ecp_core::graph::ZeroCopyGraph {
    graph_of(&JavaProvider::new().unwrap(), files)
}

fn import_scoped() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

fn global() -> f32 {
    ResolutionTier::Global.base_confidence()
}

#[test]
fn test_resolve_call_class_import_returns_import_scoped() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import pkg.Helper; class App { void go() { Helper.helper(); } }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(HELPER.0, "Helper", "helper", import_scoped())],
    );
}

#[test]
fn test_resolve_call_static_import_returns_import_scoped() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static pkg.Helper.helper; class App { void go() { helper(); } }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(HELPER.0, "Helper", "helper", import_scoped())],
    );
}

#[test]
fn test_resolve_call_static_import_missing_member_keeps_global() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static pkg.Empty.helper; class App { void go() { helper(); } }",
        ),
        (
            "src/main/java/pkg/Empty.java",
            "package pkg; class Empty {}",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "Helper", "helper", global())]);
}

#[test]
fn test_resolve_call_static_wildcard_import_returns_import_scoped() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static pkg.Helper.*; class App { void go() { helper(); } }",
        ),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[(HELPER.0, "Helper", "helper", import_scoped())],
    );
}

#[test]
fn test_resolve_call_external_static_import_returns_no_edge() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static external.Other.helper; class App { void go() { helper(); } }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[]);
}

#[test]
fn test_resolve_call_external_import_with_indexed_root_keeps_global() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static external.Other.helper; class App { void go() { helper(); } }",
        ),
        ("external/marker.java", ""),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "Helper", "helper", global())]);
}

/// Wildcards never suppress: the external star import leaves the global tier.
#[test]
fn test_resolve_call_external_wildcard_import_keeps_global() {
    let graph = java_graph(&[
        HELPER,
        (
            "App.java",
            "import static external.Other.*; class App { void go() { helper(); } }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "Helper", "helper", global())]);
}

/// Two source roots place `pkg.Helper`: the import is ambiguous, so the
/// call keeps the global tier instead of picking a root.
#[test]
fn test_resolve_call_import_with_two_roots_keeps_global() {
    let graph = java_graph(&[
        HELPER,
        (
            "src/test/java/pkg/Helper.java",
            "package pkg; public class Helper {}",
        ),
        (
            "App.java",
            "import static pkg.Helper.helper; class App { void go() { helper(); } }",
        ),
    ]);
    assert_call_targets(&graph, "go", &[(HELPER.0, "Helper", "helper", global())]);
}

/// A static import names a bare call only: a member called on a computed
/// receiver is not `format` from the import. The external import used to
/// suppress the repo's only `format`.
#[test]
fn test_resolve_call_computed_receiver_ignores_static_import_keeps_global() {
    let graph = java_graph(&[
        (
            "src/main/java/pkg/Fmt.java",
            "package pkg; public class Fmt { public String format() { return null; } }",
        ),
        (
            "App.java",
            "import static ext.Lib.format; class App { void go() { make().fmt().format(); } }",
        ),
    ]);
    let hits = calls_from(&graph, "go");
    assert!(
        hits.iter()
            .any(|h| h.name == "format" && h.file == "src/main/java/pkg/Fmt.java"),
        "{hits:?}"
    );
}

/// A Java import of a Kotlin class: discovery in Java files alone misses,
/// but the Kotlin file exists, so the call keeps its qualifier path.
#[test]
fn test_resolve_call_import_of_kotlin_class_keeps_qualifier_edge() {
    let mut builder = GraphBuilder::new();
    builder.add_graph(
        KotlinProvider::new()
            .unwrap()
            .parse_file(
                Path::new("pkg/KtThing.kt"),
                b"package pkg\nobject KtThing {\n fun foo() {}\n}\n",
            )
            .unwrap(),
    );
    builder.add_graph(
        JavaProvider::new()
            .unwrap()
            .parse_file(
                Path::new("App.java"),
                b"import pkg.KtThing; class App { void go() { KtThing.foo(); } }",
            )
            .unwrap(),
    );
    let hits = calls_from(&builder.build(), "go");
    assert!(
        hits.len() == 1 && hits[0].name == "foo" && hits[0].file == "pkg/KtThing.kt",
        "{hits:?}"
    );
}

// ── Owner of a Java import's member (FU-2026-10-05-e838f64395a2) ──────────
// A member records only its owner's last segment, so the import tier must
// pin the owner from the import path: a static member import names its
// class, and a nested class import names every enclosing class.

/// The `Calls` edges from `caller`, each as (file, owner, name, 1-based
/// line, confidence): the line tells two same-named members apart.
fn assert_call_lines(
    graph: &ZeroCopyGraph,
    caller: &str,
    expected: &[(&str, &str, &str, u32, f32)],
) {
    let pool = graph.string_pool.as_slice();
    let actual: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::Calls
                && graph.nodes[e.source as usize].name.resolve(pool) == caller
        })
        .map(|e| {
            let target = &graph.nodes[e.target as usize];
            (
                graph.files[target.file_idx as usize].path.resolve(pool),
                target.owner_class.resolve(pool),
                target.name.resolve(pool),
                target.span.0 + 1,
                e.confidence,
            )
        })
        .collect();
    assert_eq!(actual, expected, "`{caller}` Calls edges");
}

/// `Other.run` is declared first, so a lookup without an owner finds it.
const UTIL_AFTER_OTHER: (&str, &str) = (
    "r/Util.java",
    "package r;\nclass Other { static void run(){} }\npublic class Util { public static void run(){} }\n",
);

#[test]
fn test_resolve_call_static_import_owner_declared_second_binds_named_class() {
    let graph = java_graph(&[
        UTIL_AFTER_OTHER,
        (
            "q/C2.java",
            "package q;\nimport static r.Util.run;\nclass C2 { void go() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go",
        &[("r/Util.java", "Util", "run", 3, import_scoped())],
    );
}

/// Pass 1 keys a member by its owner's last segment, so `Outer1.Helper.run`
/// collapses into `Outer2.Helper.run` (line 2) and has no node of its own.
/// The import tier must not bind line 2, and the receiver ladder must not
/// either: the file declares `Helper` twice, so no owner key pins the type.
#[test]
fn test_resolve_call_nested_class_import_same_named_owner_returns_no_edge() {
    let graph = java_graph(&[
        (
            "h/Outer1.java",
            "package h;\nclass Outer2 { static class Helper { static void run(){} } }\npublic class Outer1 { static class Helper { static void run(){} } }\n",
        ),
        (
            "q/C3.java",
            "package q;\nimport h.Outer1.Helper;\nclass C3 { void go3() { Helper.run(); } }\n",
        ),
    ]);
    assert_call_lines(&graph, "go3", &[]);
}

#[test]
fn test_resolve_call_static_wildcard_import_owner_declared_second_binds_named_class() {
    let graph = java_graph(&[
        UTIL_AFTER_OTHER,
        (
            "q/C4.java",
            "package q;\nimport static r.Util.*;\nclass C4 { void go4() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go4",
        &[("r/Util.java", "Util", "run", 3, import_scoped())],
    );
}

/// `Util` declares no `run`. Java allows a static import of an inherited
/// static member, so the import tier leaves the call to the general tiers,
/// which find the repo's only `run`; the import tier never binds `Other.run`.
#[test]
fn test_resolve_call_static_import_member_not_declared_by_class_keeps_global() {
    let graph = java_graph(&[
        (
            "r/Util.java",
            "package r;\nclass Other { static void run(){} }\npublic class Util {}\n",
        ),
        (
            "q/C5.java",
            "package q;\nimport static r.Util.run;\nclass C5 { void go5() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go5",
        &[("r/Util.java", "Other", "run", 2, global())],
    );
}

/// Both `Util.run` overloads are nodes: the import binds the named class
/// `Util`, and the call reaches each overload.
#[test]
fn test_resolve_call_static_import_overloaded_member_binds_named_class() {
    let graph = java_graph(&[
        (
            "r/Util.java",
            "package r;\nclass Other { static void run(){} }\npublic class Util { static void run(){} static void run(int x){} }\n",
        ),
        (
            "q/C7.java",
            "package q;\nimport static r.Util.run;\nclass C7 { void go7() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go7",
        &[
            ("r/Util.java", "Util", "run", 3, import_scoped()),
            ("r/Util.java", "Util", "run", 3, import_scoped()),
        ],
    );
}

const NESTED_THREE_DEEP: (&str, &str) = (
    "a/B.java",
    "package a;\nclass Other { static void run(){} }\npublic class B { static class C { static class D { static void run(){} } } }\n",
);

#[test]
fn test_resolve_call_static_import_three_level_nested_owner_binds_innermost_class() {
    let graph = java_graph(&[
        NESTED_THREE_DEEP,
        (
            "q/C6.java",
            "package q;\nimport static a.B.C.D.run;\nclass C6 { void go6() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go6",
        &[("a/B.java", "D", "run", 3, import_scoped())],
    );
}

#[test]
fn test_resolve_call_three_level_nested_class_import_binds_innermost_class() {
    let graph = java_graph(&[
        NESTED_THREE_DEEP,
        (
            "q/C8.java",
            "package q;\nimport a.B.C.D;\nclass C8 { void go8() { D.run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go8",
        &[("a/B.java", "D", "run", 3, import_scoped())],
    );
}

#[test]
fn test_resolve_call_class_import_owner_declared_second_binds_named_class() {
    let graph = java_graph(&[
        UTIL_AFTER_OTHER,
        (
            "q/C9.java",
            "package q;\nimport r.Util;\nclass C9 { void go9() { Util.run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go9",
        &[("r/Util.java", "Util", "run", 3, import_scoped())],
    );
}

/// A package-private `Util` declared in `r/Helpers.java`: the import path
/// names no file, so the import tier binds nothing and the call keeps the
/// general tiers. The import tier does not search the package for the owner.
#[test]
fn test_resolve_call_static_import_owner_in_unnamed_file_keeps_global() {
    let graph = java_graph(&[
        (
            "r/Helpers.java",
            "package r;\nclass Helpers {}\nclass Util { static void run(){} }\n",
        ),
        (
            "r/App.java",
            "package r;\nimport static r.Util.run;\nclass App { void go10() { run(); } }\n",
        ),
    ]);
    assert_call_lines(
        &graph,
        "go10",
        &[("r/Helpers.java", "Util", "run", 3, global())],
    );
}
