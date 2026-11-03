//! FU-2026-11-01-c07a40e90e36, dynamic module load: Go `plugin.Open` + `Lookup`.
//!
//! Load form: `plugin.Open("x.so")` + `p.Lookup("Run")`, then a call of the
//! looked-up symbol. `src/go/parser.rs:503` only emits the `go-plugin-open` blind
//! spot. The looked-up name is a string, and the call is on a type assertion, so
//! nothing names the project `Run`. Expected: no `Calls` edge to the project
//! `Run` declared in the same package.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
use crate::instantiation_calls_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&GoProvider::new().expect("GoProvider::new"), files)
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
    "cmd/other.go",
    "package main\n\nfunc Run() {}\n\nfunc Plain() {}\n",
);

const LOADER: (&str, &str) = (
    "cmd/main.go",
    r#"package main

import "plugin"

func load() {
	p, _ := plugin.Open("x.so")
	sym, _ := p.Lookup("Run")
	sym.(func())()
}
"#,
);

#[test]
fn test_plugin_open_lookup_binds_no_project_run() {
    let g = graph(&[DECOY, LOADER]);
    assert_eq!(
        calls_into(&g, "load", DECOY.0),
        Vec::<String>::new(),
        "load calls: {:?}",
        calls_from(&g, "load")
    );
}

/// Control: a same-package call to a unique name binds.
#[test]
fn test_plugin_open_control_same_package_call_binds() {
    let g = graph(&[
        DECOY,
        ("cmd/ctl.go", "package main\n\nfunc control() { Plain() }\n"),
    ]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".Plain".to_string()]
    );
}
