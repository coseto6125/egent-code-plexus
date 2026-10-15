//! FU-2026-10-05-e838f64395a2 gives a Java static member import the owner
//! named by its path. A Kotlin import of `pkg.a.helper` may name a top-level
//! function or an object member, so a Kotlin member import keeps binding the
//! first declaration of the name in the module; these tests pin that.
mod instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::heuristics::ResolutionTier;
use instantiation_calls_support::{assert_call_targets, graph_of};

fn kotlin_graph(files: &[(&str, &str)]) -> ecp_core::graph::ZeroCopyGraph {
    graph_of(&KotlinProvider::new().unwrap(), files)
}

fn import_scoped() -> f32 {
    ResolutionTier::ImportScoped.base_confidence()
}

/// An owner taken from the path (`pkg`) would match no member here.
#[test]
fn test_resolve_call_kotlin_top_level_member_import_returns_import_scoped() {
    let graph = kotlin_graph(&[
        (
            "pkg/Util.kt",
            "package pkg\nfun helper() {}\nclass Other {\n fun helper() {}\n}\n",
        ),
        ("App.kt", "import pkg.helper\nfun go() { helper() }"),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/Util.kt", "", "helper", import_scoped())],
    );
}

#[test]
fn test_resolve_call_kotlin_object_member_import_returns_import_scoped() {
    let graph = kotlin_graph(&[
        (
            "pkg/Obj.kt",
            "package pkg\nobject Obj {\n fun refresh() {}\n}\n",
        ),
        ("App.kt", "import pkg.Obj.refresh\nfun go() { refresh() }"),
    ]);
    assert_call_targets(
        &graph,
        "go",
        &[("pkg/Obj.kt", "Obj", "refresh", import_scoped())],
    );
}
