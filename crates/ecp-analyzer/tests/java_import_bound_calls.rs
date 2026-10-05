//! A Java import that names a repo file binds the callee to that file's
//! member (ImportScoped); one that names nothing local leaves no edge; any
//! other import keeps the resolution path the call had before the import tier.
mod instantiation_calls_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use ecp_core::analyzer::provider::LanguageProvider;
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
