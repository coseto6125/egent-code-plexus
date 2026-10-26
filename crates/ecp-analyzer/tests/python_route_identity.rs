//! Python route identity: one Route node per (file, verb, path) (C1 `RouteKey`),
//! and the handler edge starts at the decorated declaration (C4 `RawRoute.handler`).

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::route_detector::{BS_KIND_ROUTE_HANDLER_ABSENT, BS_KIND_ROUTE_UNBOUND_HANDLER};
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::identity::HandlerRef;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// Parse through the pipeline, which binds record endpoints, then build.
fn build(path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
    let graph = pipeline
        .parse_file_raw(Path::new(path), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(graph);
    builder.build()
}

fn name(g: &ZeroCopyGraph, idx: u32) -> &str {
    g.nodes[idx as usize].name.resolve(&g.string_pool)
}

/// Every Route node's name, sorted, duplicates kept.
fn route_names(g: &ZeroCopyGraph) -> Vec<&str> {
    let mut names: Vec<&str> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].kind == NodeKind::Route)
        .map(|i| name(g, i))
        .collect();
    names.sort_unstable();
    names
}

/// `(handler name, handler start line, printed handler kind, edge reason)`
/// of each `HandlesRoute` edge into the one Route node named `route`.
fn handlers(g: &ZeroCopyGraph, route: &str) -> Vec<(String, u32, &'static str, String)> {
    let targets: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].kind == NodeKind::Route && name(g, i) == route)
        .collect();
    assert_eq!(
        targets.len(),
        1,
        "one Route node `{route}`: {:?}",
        route_names(g)
    );
    let mut out: Vec<_> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::HandlesRoute && e.target == targets[0])
        .map(|e| {
            let source = &g.nodes[e.source as usize];
            let handler = name(g, e.source);
            let uid = source.uid;
            let printed = match handler.starts_with("<anonymous:") {
                true => HandlerRef::Closure { uid },
                false => HandlerRef::Declaration { uid },
            };
            (
                handler.to_string(),
                source.span.0 + 1,
                printed.handler_kind(),
                e.reason.resolve(&g.string_pool).to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The kinds of the route-handler BlindSpots.
fn handler_spot_kinds(g: &ZeroCopyGraph) -> Vec<String> {
    g.blind_spots
        .iter()
        .map(|bs| bs.kind.resolve(&g.string_pool).to_string())
        .filter(|kind| {
            kind == BS_KIND_ROUTE_UNBOUND_HANDLER || kind == BS_KIND_ROUTE_HANDLER_ABSENT
        })
        .collect()
}

/// `items` is a class member first and a top-level function second. The
/// decorator decorates the top-level function; HEAD bound the first node
/// named `items`, the member.
#[test]
fn test_route_python_two_same_name_handlers_binds_decorated_function() {
    let src = r#"from fastapi import FastAPI

app = FastAPI()


class Admin:
    def items(self):
        return "admin"


@app.get("/items")
def items():
    return "top"


@app.post("/items")
def create():
    return 1
"#;
    let g = build("app.py", src);
    assert_eq!(
        route_names(&g),
        vec!["GET /items", "POST /items"],
        "one Route node per (file, verb, path)"
    );
    assert_eq!(
        handlers(&g, "GET /items"),
        vec![("items".into(), 12, "named", "call-arg".into())]
    );
    assert_eq!(
        handlers(&g, "POST /items"),
        vec![("create".into(), 17, "named", "call-arg".into())]
    );
    assert_eq!(handler_spot_kinds(&g), Vec::<String>::new());
}

/// A decorator on a class member decorates that member, although a
/// top-level `status` comes first in the file. HEAD bound the top-level one.
#[test]
fn test_route_python_decorated_member_binds_member_not_top_level() {
    let src = r#"from fastapi import FastAPI

app = FastAPI()


def status():
    return 0


class Api:
    @app.get("/status")
    def status(self):
        return 1
"#;
    let g = build("app.py", src);
    assert_eq!(route_names(&g), vec!["GET /status"]);
    assert_eq!(
        handlers(&g, "GET /status"),
        vec![("status".into(), 12, "named", "call-arg".into())]
    );
}
