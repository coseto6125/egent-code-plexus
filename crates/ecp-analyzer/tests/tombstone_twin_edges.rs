//! A redefined Python function and a Rust `#[cfg]` twin are uid collisions:
//! pass 1 keeps the first definition and tombstones the second, whose name
//! is empty. The post-parse passes
//! that take a raw index (decorators, route decorators, transaction scopes)
//! map it through `RawIndexMap::node`, so the second definition's edges land
//! on the survivor. An edge at a tombstone shows an LLM a blank caller.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const TWIN_SRC: &str = r#"from django.db import transaction
from flask import Flask

app = Flask(__name__)


def cache(f):
    return f


@cache
@app.route("/a")
@transaction.atomic
def load():
    return 1


@cache
@app.route("/b")
@transaction.atomic
def load():
    return 2
"#;

/// Rust attribute routes take the decorator path (`reason = decorator`);
/// Python's `@app.route` comes from the call-argument route records.
const RUST_TWIN_SRC: &str = r#"use actix_web::get;

#[cfg(feature = "a")]
#[get("/a")]
async fn load() -> &'static str {
    "a"
}

#[cfg(not(feature = "a"))]
#[get("/b")]
async fn load() -> &'static str {
    "b"
}
"#;

fn build(provider: Box<dyn LanguageProvider>, path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(provider);
    let graph = pipeline
        .parse_file_raw(Path::new(path), src.as_bytes())
        .expect("parse_file_raw");
    let mut builder = GraphBuilder::new();
    builder.add_graph(graph);
    builder.build()
}

/// `(name, node index)` of the `Function` endpoint, other than `cache`, of
/// every `rel` edge.
fn function_endpoints(g: &ZeroCopyGraph, rel: RelType) -> Vec<(String, u32)> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel)
        .flat_map(|e| [e.source, e.target])
        .filter_map(|idx| {
            let n = &g.nodes[idx as usize];
            let name = n.name.resolve(pool);
            (n.kind == NodeKind::Function && name != "cache").then(|| (name.to_string(), idx))
        })
        .collect();
    out.sort();
    out
}

fn build_python(src: &str) -> ZeroCopyGraph {
    build(
        Box::new(PythonProvider::new().expect("PythonProvider::new")),
        "app.py",
        src,
    )
}

/// The node index of the first `load`, the survivor.
fn survivor(g: &ZeroCopyGraph) -> u32 {
    let pool = g.string_pool.as_slice();
    g.nodes
        .iter()
        .position(|n| n.kind == NodeKind::Function && n.name.resolve(pool) == "load")
        .expect("the first `load` survives") as u32
}

#[test]
fn test_python_redefined_function_decorates_edges_start_at_survivor() {
    let g = build_python(TWIN_SRC);
    let load = ("load".to_string(), survivor(&g));
    let decorated = function_endpoints(&g, RelType::Decorates);
    assert!(
        decorated.len() >= 2,
        "both definitions' decorators emit Decorates edges: {decorated:?}"
    );
    assert!(
        decorated.iter().all(|endpoint| *endpoint == load),
        "every Decorates edge reaches the first `load`, none an empty-named tombstone: {decorated:?}"
    );
}

#[test]
fn test_rust_cfg_twin_route_attributes_handlers_are_survivor() {
    let g = build(
        Box::new(RustProvider::new().expect("RustProvider::new")),
        "src/main.rs",
        RUST_TWIN_SRC,
    );
    let load = ("load".to_string(), survivor(&g));
    assert_eq!(
        function_endpoints(&g, RelType::HandlesRoute),
        vec![load.clone(), load],
        "`GET /a` and `GET /b` both name the surviving `load` as handler"
    );
}

#[test]
fn test_python_redefined_function_tx_scopes_open_at_survivor() {
    let g = build_python(TWIN_SRC);
    let load = ("load".to_string(), survivor(&g));
    let openers = function_endpoints(&g, RelType::OpensTxScope);
    assert!(
        !openers.is_empty(),
        "the fixture must emit OpensTxScope edges"
    );
    assert!(
        openers.iter().all(|opener| *opener == load),
        "every transaction scope opens at the surviving `load`: {openers:?}"
    );
}
