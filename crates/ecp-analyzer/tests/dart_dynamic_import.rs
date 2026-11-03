//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): Dart `import .. deferred as a` + `a.loadLibrary()`.
//!
//! Load form: `import 'a.dart' deferred as a;` then `await a.loadLibrary(); a.run();`.
//! A deferred import IS static: the library path is a literal, so the file gets
//! an `Imports` edge like any other import. The call `a.run()` goes through the
//! import prefix `a`; `src/dart/receiver_types.rs:360` leaves a prefix receiver
//! unbound (the bare member name is returned) and design_resolver.md N11 states
//! that prefix calls bind nothing. With a decoy `run` in `b.dart` the bare name
//! is ambiguous too, so the expected result is: an `Imports` edge to `a.dart`,
//! and no `Calls` edge to either `run`. A future fix that binds `a.run` to
//! `a.dart` is a change, not a regression (N11).
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&DartProvider::new().expect("DartProvider::new"), files)
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

const TARGET: (&str, &str) = ("a.dart", "void run() {}\n");
const DECOY: (&str, &str) = ("b.dart", "void run() {}\nvoid plain() {}\n");

const LOADER: (&str, &str) = (
    "main.dart",
    r#"import 'a.dart' deferred as a;

Future<void> load() async {
  await a.loadLibrary();
  a.run();
}
"#,
);

#[test]
fn test_deferred_import_has_static_imports_edge() {
    let g = graph(&[TARGET, DECOY, LOADER]);
    assert_eq!(imports_of(&g, "main.dart"), vec!["a.dart".to_string()]);
}

#[test]
fn test_deferred_import_prefix_call_binds_no_run() {
    let g = graph(&[TARGET, DECOY, LOADER]);
    let calls = calls_from(&g, "load");
    assert!(
        calls.iter().all(|hit| hit.name != "run"),
        "N11 pin: prefix calls bind nothing; load calls: {calls:?}"
    );
}

/// Control: a plain cross-file call to a unique name binds.
#[test]
fn test_deferred_import_control_plain_cross_file_call_binds() {
    let g = graph(&[DECOY, ("ctl.dart", "void control() { plain(); }\n")]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
