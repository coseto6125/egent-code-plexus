//! ReadsField edge: a function/method that reads a struct field emits a
//! `ReadsField` edge to the Property target. End-to-end check that the parser
//! captures the read, the resolver wires it to the Property node, and the
//! builder emits the edge.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType};
use std::path::Path;

fn build_graph(path: &str, src: &str) -> ecp_core::graph::ZeroCopyGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    let local = provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file");
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn node_name(g: &ecp_core::graph::ZeroCopyGraph, idx: u32) -> String {
    g.nodes[idx as usize]
        .name
        .resolve(&g.string_pool)
        .to_string()
}

#[test]
fn rust_field_read_emits_reads_field_edge() {
    let src = r#"
pub struct Config {
    pub timeout: u32,
}

fn read_timeout(c: &Config) -> u32 {
    c.timeout
}
"#;
    let g = build_graph("config.rs", src);

    let reads: Vec<&ecp_core::graph::Edge> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::ReadsField)
        .collect();

    assert!(
        !reads.is_empty(),
        "expected at least one ReadsField edge; got none.\nnodes: {:?}",
        g.nodes
            .iter()
            .map(|n| (n.name.resolve(&g.string_pool), &n.kind))
            .collect::<Vec<_>>()
    );

    let edge = reads[0];
    assert_eq!(
        node_name(&g, edge.source),
        "read_timeout",
        "ReadsField source must be the reading function"
    );
    assert_eq!(
        node_name(&g, edge.target),
        "timeout",
        "ReadsField target must be the field"
    );
    assert_eq!(
        g.nodes[edge.target as usize].kind,
        NodeKind::Property,
        "ReadsField target must be a Property node"
    );
}

/// A method call `c.compute()` is a Calls edge, not a ReadsField — `is_call_callee`
/// must exclude the callee, and even if it slipped through, the resolver's
/// is_property filter has no Property named `compute` to land on. Either way
/// no ReadsField edge to a method should appear.
#[test]
fn rust_method_call_is_not_a_field_read() {
    let src = r#"
pub struct Config { pub timeout: u32 }
impl Config {
    pub fn compute(&self) -> u32 { self.timeout }
}
fn driver(c: &Config) -> u32 {
    c.compute()
}
"#;
    let g = build_graph("config.rs", src);
    let bad = g
        .edges
        .iter()
        .any(|e| e.rel_type == RelType::ReadsField && node_name(&g, e.target) == "compute");
    assert!(
        !bad,
        "method call `compute` must not produce a ReadsField edge"
    );
}
