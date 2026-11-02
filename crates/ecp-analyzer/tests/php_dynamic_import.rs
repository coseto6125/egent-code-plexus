//! FU-2026-11-01-c07a40e90e36 (T1), T1 red-proof (design_resolver.md:133): PHP `require __DIR__ . '/a.php'`.
//!
//! Load form: `require __DIR__ . '/a.php'; run();` with a decoy `run` in `b.php`.
//! The require target is a concatenation, not a string literal, and the PHP
//! parser emits no `Imports` edge for it (design_resolver.md N13, FU N13). With
//! no import and two global functions named `run`, the bare call is ambiguous
//! (resolver.rs: a Tier 3 pick with two or more candidates is suppressed), so the
//! expected result is: no `Imports` edge from `app.php`, and no `Calls` edge to
//! either `run`. A future fix that binds `run` to `a.php` through the require
//! (N13) is a change, not a regression. Pin derived from reading, not run.
//!
//! Contract the pins stand for: a runtime load with a non-static target
//! gives no import-tier call edge; a future fix that adds one with a proven
//! target is a change, not a regression. Each file declares a decoy with the
//! loaded name, so a unique-name fallback cannot bind it by accident. Every
//! "no edge" pin has a control test that proves the harness sees a plain
//! cross-file call in the same language.
mod instantiation_calls_support;

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use instantiation_calls_support::{calls_from, edges_from, graph_of};

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    graph_of(&PhpProvider::new().expect("PhpProvider::new"), files)
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

const TARGET: (&str, &str) = ("a.php", "<?php\nfunction run() {}\n");
const DECOY: (&str, &str) = ("b.php", "<?php\nfunction run() {}\nfunction plain() {}\n");

const LOADER: (&str, &str) = (
    "app.php",
    r#"<?php
require __DIR__ . '/a.php';

function load() {
    run();
}
"#,
);

#[test]
fn test_require_dir_concat_emits_no_imports_edge() {
    let g = graph(&[TARGET, DECOY, LOADER]);
    assert_eq!(imports_of(&g, "app.php"), Vec::<String>::new());
}

#[test]
fn test_require_dir_concat_bare_call_binds_neither_run() {
    let g = graph(&[TARGET, DECOY, LOADER]);
    let calls = calls_from(&g, "load");
    assert!(
        calls.iter().all(|hit| hit.name != "run"),
        "N13 pin: load calls: {calls:?}"
    );
}

/// Control: a plain cross-file call to a unique name binds.
#[test]
fn test_require_dir_concat_control_plain_cross_file_call_binds() {
    let g = graph(&[
        DECOY,
        ("ctl.php", "<?php\nfunction control() { plain(); }\n"),
    ]);
    assert_eq!(
        calls_into(&g, "control", DECOY.0),
        vec![".plain".to_string()]
    );
}
