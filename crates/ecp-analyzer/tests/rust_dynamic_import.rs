//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): Rust `libloading::Library::new` + `get`.
//!
//! Load form: `libloading::Library::new("x.so")` + `lib.get(b"run")`, then a
//! call through the loaded symbol (named `entry`, so no declaration of that name
//! exists). `src/rust/parser.rs:158` emits only the `rs-libloading-get` blind
//! spot. Expected: no `Calls` edge into the decoy `run` of another file.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&RustProvider::new().expect("RustProvider::new"), files)
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

const DECOY: (&str, &str) = ("src/decoy.rs", "pub fn run() {}\npub fn plain() {}\n");

const LOADER: (&str, &str) = (
    "src/main.rs",
    r#"fn load() {
    let lib = unsafe { libloading::Library::new("x.so").unwrap() };
    let entry: libloading::Symbol<unsafe extern "C" fn()> = unsafe { lib.get(b"run").unwrap() };
    unsafe { entry() };
}
"#,
);

#[test]
fn test_libloading_get_binds_no_project_run() {
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
fn test_libloading_control_plain_cross_file_call_binds() {
    let g = graph(&[DECOY, ("src/ctl.rs", "fn control() { plain(); }\n")]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
