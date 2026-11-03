//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): Java `Class.forName(..).getMethod(..).invoke(..)`.
//!
//! Load form: `Class.forName("pkg.X").getMethod("run").invoke(null)`.
//! `src/java/parser.rs:173` emits a blind spot for `Class.forName`; the class and
//! method names are strings. The calls the parser sees are `forName`,
//! `getMethod` and `invoke`, none declared by the project. The file has no
//! `import pkg.X`. Expected: no edge to `pkg.X.run`.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&JavaProvider::new().expect("JavaProvider::new"), files)
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
    "src/main/java/pkg/X.java",
    "package pkg; public class X { public static void run() {} public static void plain() {} }",
);

const LOADER: (&str, &str) = (
    "src/main/java/app/App.java",
    r#"package app;
class App {
    void load() throws Exception {
        Class.forName("pkg.X").getMethod("run").invoke(null);
    }
}
"#,
);

#[test]
fn test_class_forname_reflection_binds_no_project_run() {
    let g = graph(&[DECOY, LOADER]);
    assert_eq!(
        calls_into(&g, "load", DECOY.0),
        Vec::<String>::new(),
        "load calls: {:?}",
        calls_from(&g, "load")
    );
}

/// Control: a static call through an import binds.
#[test]
fn test_class_forname_control_imported_static_call_binds() {
    let g = graph(&[
        DECOY,
        (
            "src/main/java/app/Ctl.java",
            "package app; import pkg.X; class Ctl { void control() { X.plain(); } }",
        ),
    ]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec!["X.plain".to_string()]
    );
}
