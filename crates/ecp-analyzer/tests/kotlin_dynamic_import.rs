//! FU-2026-11-01-c07a40e90e36, dynamic module load: Kotlin
//! `Class.forName(..).kotlin` reflection.
//!
//! Load form: `Class.forName("pkg.X").kotlin.members.first { .. }.call()`.
//! `src/kotlin/parser.rs:280` emits a blind spot for `Class.forName`. The member
//! is picked by a string compare inside a lambda, so nothing names the project
//! `X.run`. The file has no `import pkg.X`. Expected: no edge into `X.kt`.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&KotlinProvider::new().expect("KotlinProvider::new"), files)
}

/// `owner.name` of every `Calls` edge from `caller` into `file`.
fn calls_into(graph: &ZeroCopyGraph, caller: &str, file: &str) -> Vec<String> {
    calls_from(graph, caller)
        .into_iter()
        .filter(|hit| hit.file == file)
        .map(|hit| format!("{}.{}", hit.owner, hit.name))
        .collect()
}

/// Target files of every `Imports` edge of the File node named `file`.
#[allow(dead_code)]
fn imports_of(graph: &ZeroCopyGraph, file: &str) -> Vec<String> {
    edges_from(graph, file, RelType::Imports)
        .into_iter()
        .map(|hit| hit.file)
        .collect()
}

const DECOY: (&str, &str) = (
    "src/main/kotlin/pkg/X.kt",
    "package pkg\n\nobject X {\n    fun run() {}\n    fun plain() {}\n}\n",
);

const LOADER: (&str, &str) = (
    "src/main/kotlin/app/App.kt",
    r#"package app

fun load() {
    Class.forName("pkg.X").kotlin.members.first { it.name == "run" }.call()
}
"#,
);

#[test]
fn test_class_forname_kotlin_reflection_binds_no_project_run() {
    let g = graph(&[DECOY, LOADER]);
    assert_eq!(
        calls_into(&g, "load", DECOY.0),
        Vec::<String>::new(),
        "load calls: {:?}",
        calls_from(&g, "load")
    );
}

/// Control: a call through an import binds.
#[test]
fn test_class_forname_control_imported_object_call_binds() {
    let g = graph(&[
        DECOY,
        (
            "src/main/kotlin/app/Ctl.kt",
            "package app\n\nimport pkg.X\n\nfun control() { X.plain() }\n",
        ),
    ]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec!["X.plain".to_string()]
    );
}
