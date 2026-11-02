//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): C++ `dlopen`/`dlsym`.
//!
//! Load form: `dlopen("./libx.so", RTLD_NOW)` + `dlsym(h, "run")`, then a call through the
//! resulting function pointer. Evidence for the pin: the cpp parser shares the C call extraction; the call goes through a pointer local, not a declared name.
//! The expected result is the absence of a `Calls` edge into the decoy `run`.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
mod instantiation_calls_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&CppProvider::new().expect("CppProvider::new"), files)
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

const DECOY: (&str, &str) = ("decoy.cpp", "void run(void) {}\nvoid plain(void) {}\n");

const LOADER: (&str, &str) = (
    "main.cpp",
    r#"#include <dlfcn.h>
typedef void (*run_fn)(void);

void load(void) {
    void *h = dlopen("./libx.so", RTLD_NOW);
    run_fn entry = reinterpret_cast<run_fn>(dlsym(h, "run"));
    entry();
}
"#,
);

/// Today's result: the call goes through the pointer local `entry`, which
/// names no declaration; `dlsym`'s string argument is data, not a callee.
/// Expected: no `Calls` edge into the decoy `run`.
#[test]
fn test_dlopen_dlsym_call_through_pointer_binds_no_decoy() {
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
fn test_dlopen_dlsym_control_plain_cross_file_call_binds() {
    let g = graph(&[DECOY, ("ctl.cpp", "void control(void) { plain(); }\n")]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
