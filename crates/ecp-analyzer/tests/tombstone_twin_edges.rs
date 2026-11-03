//! A redefined Python function and a Rust `#[cfg]` twin are redefinitions: pass
//! 1 keeps each definition as a node of its own (C1-identity.md "Repeat
//! policy"), the second a later node under `overload(first, 1)` with its name.
//! The post-parse passes that take a raw index (decorators, route decorators,
//! transaction scopes) map it through `RawIndexMap::node`, so each definition's
//! edges start at its own node. Only a repeat that merges
//! (`uid::merges_repeats`: a property, variable or constant) becomes an
//! empty-named tombstone, whose edges start at the survivor (C1-identity.md
//! "Edge sources bind by row", Call source); the C `#if` variable below pins
//! it. An edge at a tombstone shows an LLM a blank caller.
//!
//! The twin needs a kind that keeps no overloads (`uid::keeps_overloads`:
//! every kind but `Method` and `Constructor`), declared twice in one file
//! by valid code, and carrying a decorator, a transaction scope or a
//! decorator route. C# and Swift reach it with a type declared in both
//! branches of `#if`, PHP with a function declared in both branches of an
//! `if`. The other mainstream languages have no row:
//! TypeScript and JavaScript put decorators only on classes and class
//! members, a member is a `Method`, and a class declared twice is an error;
//! Java has no top-level function and a type declared twice does not compile;
//! Kotlin and Dart reject a second top-level declaration of one name and have
//! no in-file conditional compilation; Go rejects a function declared twice in
//! one file, and a build tag selects whole files; Ruby makes every `def` a
//! `Method`, and a reopened class carries no decorator (the `Decorates` pass
//! skips Ruby); C and C++ redefine under `#ifdef`, but their parsers emit no
//! decorators, transaction scopes or decorator routes.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_analyzer::swift::parser::SwiftProvider;
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

/// Each branch declares `Load` with its own attribute, so each twin emits
/// one `Decorates` edge to a target of its own.
const CSHARP_TWIN_SRC: &str = r#"namespace App
{
#if NET8_0
    [Obsolete]
    public class Load
    {
    }
#else
    [Serializable]
    public class Load
    {
    }
#endif
}
"#;

const SWIFT_TWIN_SRC: &str = r#"#if os(iOS)
@objc
class Load {
}
#else
@available(macOS 10.15, *)
class Load {
}
#endif
"#;

const PHP_TWIN_SRC: &str = r#"<?php

if (PHP_VERSION_ID >= 80000) {
    #[Pure]
    function load() {
        return 1;
    }
} else {
    #[Deprecated]
    function load() {
        return 2;
    }
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

/// `(name, node index)` of the `kind` endpoint, other than `cache`, of
/// every `rel` edge.
fn endpoints(g: &ZeroCopyGraph, rel: RelType, kind: NodeKind) -> Vec<(String, u32)> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel)
        .flat_map(|e| [e.source, e.target])
        .filter_map(|idx| {
            let n = &g.nodes[idx as usize];
            let name = n.name.resolve(pool);
            (n.kind == kind && name != "cache").then(|| (name.to_string(), idx))
        })
        .collect();
    out.sort();
    out
}

fn function_endpoints(g: &ZeroCopyGraph, rel: RelType) -> Vec<(String, u32)> {
    endpoints(g, rel, NodeKind::Function)
}

fn build_python(src: &str) -> ZeroCopyGraph {
    build(
        Box::new(PythonProvider::new().expect("PythonProvider::new")),
        "app.py",
        src,
    )
}

/// The node indices of every `kind` node named `name`, ascending.
fn named_nodes(g: &ZeroCopyGraph, kind: NodeKind, name: &str) -> Vec<u32> {
    let pool = g.string_pool.as_slice();
    (0..g.nodes.len() as u32)
        .filter(|&i| {
            let n = &g.nodes[i as usize];
            n.kind == kind && n.name.resolve(pool) == name
        })
        .collect()
}

/// `(name, idx)` of each of the two `kind` nodes named `name`.
fn both(g: &ZeroCopyGraph, kind: NodeKind, name: &str) -> Vec<(String, u32)> {
    let nodes = named_nodes(g, kind, name);
    assert_eq!(
        nodes.len(),
        2,
        "both `{name}` definitions keep a named node"
    );
    nodes.into_iter().map(|i| (name.to_string(), i)).collect()
}

/// Each twin's one decorator emits one `Decorates` edge, from the twin's
/// own node. Before the change the second edge started at the first, the
/// survivor of a tombstone.
fn assert_twin_decorates_at_own_node(g: &ZeroCopyGraph, kind: NodeKind, name: &str) {
    assert_eq!(
        endpoints(g, RelType::Decorates, kind),
        both(g, kind, name),
        "each twin's decorator reaches its own `{name}`, none an empty-named tombstone"
    );
}

/// The distinct `Function` endpoints of `rel`, which must name both `load`s.
fn assert_each_load_once_or_more(g: &ZeroCopyGraph, rel: RelType) {
    let mut seen = function_endpoints(g, rel);
    seen.dedup();
    assert_eq!(
        seen,
        both(g, NodeKind::Function, "load"),
        "{rel:?} edges reach each `load`'s own node"
    );
}

/// Stands for: each redefinition keeps its own decorators (C1-identity.md
/// "Repeat policy").
#[test]
fn test_python_redefined_function_decorates_edges_start_at_own_node() {
    let g = build_python(TWIN_SRC);
    assert_each_load_once_or_more(&g, RelType::Decorates);
}

/// Stands for: `GET /a` and `GET /b` each name the `load` that declares
/// them.
#[test]
fn test_rust_cfg_twin_route_attributes_handlers_are_own_node() {
    let g = build(
        Box::new(RustProvider::new().expect("RustProvider::new")),
        "src/main.rs",
        RUST_TWIN_SRC,
    );
    assert_eq!(
        function_endpoints(&g, RelType::HandlesRoute),
        both(&g, NodeKind::Function, "load"),
    );
}

/// Stands for: each redefinition opens its own transaction scope.
#[test]
fn test_python_redefined_function_tx_scopes_open_at_own_node() {
    let g = build_python(TWIN_SRC);
    assert_each_load_once_or_more(&g, RelType::OpensTxScope);
}

#[test]
fn test_csharp_if_twin_class_decorates_edges_start_at_own_node() {
    let g = build(
        Box::new(CSharpProvider::new().expect("CSharpProvider::new")),
        "src/Load.cs",
        CSHARP_TWIN_SRC,
    );
    assert_twin_decorates_at_own_node(&g, NodeKind::Class, "Load");
}

#[test]
fn test_swift_if_twin_class_decorates_edges_start_at_own_node() {
    let g = build(
        Box::new(SwiftProvider::new().expect("SwiftProvider::new")),
        "Sources/Load.swift",
        SWIFT_TWIN_SRC,
    );
    assert_twin_decorates_at_own_node(&g, NodeKind::Class, "Load");
}

#[test]
fn test_php_if_twin_function_decorates_edges_start_at_own_node() {
    let g = build(
        Box::new(PhpProvider::new().expect("PhpProvider::new")),
        "src/load.php",
        PHP_TWIN_SRC,
    );
    assert_twin_decorates_at_own_node(&g, NodeKind::Function, "load");
}

const C_IF_VARIABLE_SRC: &str = "#if A
int limit = 1;
#elif B
int limit = 2;
#else
int limit = 3;
#endif
";

/// Stands for: the merge class. A C variable declared in each branch of
/// `#if/#elif/#else` is one entity: one named node, two tombstones with
/// pairwise distinct uids (`tombstone(overload(base, n))`), and one
/// `Defines` edge, to the named node.
#[test]
fn test_c_if_elif_else_variable_one_named_node_distinct_uids_one_defines() {
    let g = build(
        Box::new(CProvider::new().expect("CProvider::new")),
        "src/limit.c",
        C_IF_VARIABLE_SRC,
    );
    let pool = g.string_pool.as_slice();
    let rows: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| {
            matches!(
                g.nodes[i as usize].kind,
                NodeKind::Variable | NodeKind::Const
            )
        })
        .collect();
    assert_eq!(rows.len(), 3, "one slot per declaration");
    let names: Vec<&str> = rows
        .iter()
        .map(|&i| g.nodes[i as usize].name.resolve(pool))
        .collect();
    assert_eq!(names, ["limit", "", ""]);
    let base = g.nodes[rows[0] as usize].uid;
    assert_eq!(
        rows.iter()
            .map(|&i| g.nodes[i as usize].uid)
            .collect::<Vec<_>>(),
        [
            base,
            ecp_core::uid::tombstone(ecp_core::uid::overload(base, 1)),
            ecp_core::uid::tombstone(ecp_core::uid::overload(base, 2)),
        ]
    );
    let defines: Vec<u32> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Defines && rows.contains(&e.target))
        .map(|e| e.target)
        .collect();
    assert_eq!(defines, [rows[0]], "one Defines, to the named node");
}
