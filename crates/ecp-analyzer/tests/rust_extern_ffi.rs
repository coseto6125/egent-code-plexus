//! Rust FFI extern block — `function_signature_item` inside `foreign_mod_item`.
//!
//! Verifies that functions declared in `extern "C" { fn foo(...); }` blocks
//! are emitted as `NodeKind::Function` nodes. Without the `foreign_mod_item`
//! pattern in queries.scm these nodes were invisible to the graph.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse_rs(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new("test.rs"), src.as_bytes())
        .expect("parse_file")
}

// ── basic extern "C" block ────────────────────────────────────────────────────

#[test]
fn rust_extern_c_ffi_functions_are_emitted() {
    let src = r#"
extern "C" {
    fn c_add(a: i32, b: i32) -> i32;
    fn c_free(ptr: *mut u8);
}
fn caller() { unsafe { c_add(1, 2); } }
"#;
    let g = parse_rs(src);

    let pool = g.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>();

    let c_add = g
        .nodes
        .iter()
        .find(|n| n.name == "c_add" && matches!(n.kind, NodeKind::Function));
    assert!(
        c_add.is_some(),
        "expected Function node for c_add; emitted names: {pool:?}"
    );

    let c_free = g
        .nodes
        .iter()
        .find(|n| n.name == "c_free" && matches!(n.kind, NodeKind::Function));
    assert!(
        c_free.is_some(),
        "expected Function node for c_free; emitted names: {pool:?}"
    );
}

// ── extern block with multiple ABIs ──────────────────────────────────────────

#[test]
fn rust_extern_system_ffi_functions_are_emitted() {
    let src = r#"
extern "system" {
    fn win_sleep(ms: u32);
}
"#;
    let g = parse_rs(src);
    let found = g
        .nodes
        .iter()
        .any(|n| n.name == "win_sleep" && matches!(n.kind, NodeKind::Function));
    assert!(found, "Function node for win_sleep not emitted");
}

// ── regular functions are not double-emitted ─────────────────────────────────

#[test]
fn rust_extern_ffi_does_not_duplicate_regular_functions() {
    let src = r#"
extern "C" {
    fn c_strlen(s: *const u8) -> usize;
}
fn regular_fn() {}
"#;
    let g = parse_rs(src);

    let c_strlen_count = g
        .nodes
        .iter()
        .filter(|n| n.name == "c_strlen" && matches!(n.kind, NodeKind::Function))
        .count();
    assert_eq!(c_strlen_count, 1, "c_strlen must appear exactly once");

    let regular_count = g
        .nodes
        .iter()
        .filter(|n| n.name == "regular_fn" && matches!(n.kind, NodeKind::Function))
        .count();
    assert_eq!(regular_count, 1, "regular_fn must appear exactly once");
}

// ── FFI function node has correct kind (not Method or other) ─────────────────

#[test]
fn rust_extern_c_ffi_node_kind_is_function() {
    let src = r#"
extern "C" {
    fn c_open(path: *const u8, flags: i32) -> i32;
}
"#;
    let g = parse_rs(src);
    let node = g.nodes.iter().find(|n| n.name == "c_open");
    assert!(node.is_some(), "c_open node not emitted");
    assert_eq!(
        node.unwrap().kind,
        NodeKind::Function,
        "extern C fn must have NodeKind::Function, got: {:?}",
        node.unwrap().kind
    );
}
