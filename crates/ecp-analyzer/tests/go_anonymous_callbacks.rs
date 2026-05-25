//! Calls inside an anonymous func literal passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `runtime.SetFinalizer(x, func(o *T){ cleanup(o) })` at file scope
//! produced 0 callers for `cleanup` before this change.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = GoProvider::new().expect("provider");
    p.parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

#[test]
fn func_literal_callback_attaches_call_to_anonymous_node() {
    let g = parse(
        r#"package main

import "runtime"

type T struct{}

func cleanup(o *T) {}

func register(x *T) {
    runtime.SetFinalizer(x, func(o *T) { cleanup(o) })
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"cleanup"),
        "expected cleanup attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn sort_slice_callback_attaches_call() {
    let g = parse(
        r#"package main

import "sort"

func less(a, b int) bool { return a < b }

func sortIt(s []int) {
    sort.Slice(s, func(i, j int) bool { return less(s[i], s[j]) })
}
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"less"),
        "expected less attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_func_literal_emits_no_anonymous_node() {
    let g = parse(
        r#"package main

import "sort"

func noop(s []int) {
    sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })
}
"#,
    );
    // No call_expression inside the func literal (the comparison is not a call).
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "func literal without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function by reference — no func_literal in argument_list,
    // no <anonymous> node should appear.
    let g = parse(
        r#"package main

import "sort"

func less(i, j int) bool { return i < j }

func sortIt(s []int) {
    sort.Slice(s, less)
}
"#,
    );
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
