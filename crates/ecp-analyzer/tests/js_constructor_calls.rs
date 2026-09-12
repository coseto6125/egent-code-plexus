//! JS/TS lexical call resolution must treat a `constructor` as a lexical
//! parent. Before this fix the lexical index only listed `Function | Method`,
//! so a helper declared inside a constructor had no parent and the empty
//! candidate list skipped same-file resolution: `constructor → helper` lost
//! its `Calls` edge and `ecp impact` under-reported the callers.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use std::path::Path;

const SRC: &str = "\
class C {
  constructor() {
    function helper() {}
    helper();
  }
  method() {
    function inner() {}
    inner();
  }
}
function top() {}
function caller() { top(); }
";

fn build(local: LocalGraph) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(local);
    builder.build()
}

fn call_pairs(g: &ZeroCopyGraph) -> Vec<(String, String)> {
    let name = |idx: u32| {
        g.nodes[idx as usize]
            .name
            .resolve(&g.string_pool)
            .to_string()
    };
    let mut pairs: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls)
        .map(|e| (name(e.source), name(e.target)))
        .collect();
    pairs.sort();
    pairs
}

fn assert_constructor_call_present(g: &ZeroCopyGraph) {
    let pairs = call_pairs(g);
    for expected in [
        ("constructor", "helper"),
        ("method", "inner"),
        ("caller", "top"),
    ] {
        assert!(
            pairs.contains(&(expected.0.into(), expected.1.into())),
            "missing Calls edge {expected:?}; edges: {pairs:?}"
        );
    }
}

#[test]
fn test_js_constructor_local_helper_call_keeps_calls_edge() {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    let local = provider
        .parse_file(Path::new("c.js"), SRC.as_bytes())
        .expect("parse_file");
    let helper = local
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper node");
    assert!(
        helper
            .owner_class
            .as_deref()
            .is_some_and(|o| o.contains("constructor")),
        "helper must record its constructor as lexical owner: {:?}",
        helper.owner_class
    );
    assert_constructor_call_present(&build(local));
}

#[test]
fn test_ts_constructor_local_helper_call_keeps_calls_edge() {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    let local = provider
        .parse_file(Path::new("c.ts"), SRC.as_bytes())
        .expect("parse_file");
    assert_constructor_call_present(&build(local));
}
