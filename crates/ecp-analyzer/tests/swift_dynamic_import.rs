//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): Swift `NSClassFromString` / `dlopen`.
//!
//! Load form: `NSClassFromString("X")` and `dlopen`/`dlsym`. The class and
//! symbol names are strings, and the symbol comes back as an opaque pointer.
//! Expected: no `Calls` edge into the decoy `run` of another file.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
mod instantiation_calls_support;

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&SwiftProvider::new().expect("SwiftProvider::new"), files)
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

const DECOY: (&str, &str) = ("Decoy.swift", "func run() {}\nfunc plain() {}\n");

const LOADER: (&str, &str) = (
    "Loader.swift",
    r#"import Foundation

func load() {
    let cls = NSClassFromString("X")
    let handle = dlopen("./libx.dylib", RTLD_NOW)
    let sym = dlsym(handle, "run")
    _ = (cls, sym)
}
"#,
);

#[test]
fn test_nsclassfromstring_dlopen_binds_no_project_run() {
    let g = graph(&[DECOY, LOADER]);
    assert_eq!(
        calls_into(&g, "load", DECOY.0),
        Vec::<String>::new(),
        "load calls: {:?}",
        calls_from(&g, "load")
    );
}

/// Control: a plain cross-file call to a unique name binds.
#[test]
fn test_nsclassfromstring_control_plain_cross_file_call_binds() {
    let g = graph(&[DECOY, ("Ctl.swift", "func control() { plain() }\n")]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
