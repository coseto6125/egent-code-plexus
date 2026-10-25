//! Go route identity: one Route node per (file, verb, path) (C1
//! `RouteKey`), and the handler edge starts at the handler the gin / echo
//! registration passes last (C4 `RawRoute.handler`).

use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::route_detector::{BS_KIND_ROUTE_HANDLER_ABSENT, BS_KIND_ROUTE_UNBOUND_HANDLER};
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::identity::HandlerRef;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// Parse through the pipeline, which binds record endpoints, then build.
fn build(path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(GoProvider::new().unwrap()));
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

const GIN: &str = r#"package main

import "github.com/gin-gonic/gin"

func list(c *gin.Context) {
	c.JSON(200, nil)
}

func main() {
	r := gin.Default()
	r.GET("/items", list)
	r.GET("/inline", func(c *gin.Context) {
		c.JSON(200, nil)
	})
	r.GET("/empty", func(c *gin.Context) {})
	r.Run()
}
"#;

/// A gin handler written in place has a closure node when its body calls
/// something: the edge starts at that node and prints `inline_anonymous`.
/// On HEAD the Go parser recorded no handler, so the route had no edge.
#[test]
fn test_route_gin_inline_closure_handler_kind_inline_anonymous() {
    let g = build("main.go", GIN);
    let inline = handlers(&g, "GET /inline");
    assert_eq!(inline.len(), 1, "{inline:?}");
    assert!(inline[0].0.starts_with("<anonymous:12:"), "{inline:?}");
    assert_eq!(
        (inline[0].2, inline[0].3.as_str()),
        ("inline_anonymous", "inline-closure")
    );
    // An empty closure has no node: no edge, and no BlindSpot either, so
    // `ecp routes` reads it as inline, never as unresolved or absent.
    assert!(handlers(&g, "GET /empty").is_empty());
    assert_eq!(handler_spot_kinds(&g), Vec::<String>::new());
}

/// `r.GET("/items", list)` binds the top-level `list`. HEAD recorded no
/// handler for a Go route.
#[test]
fn test_route_gin_named_handler_binds_declared_function() {
    let g = build("main.go", GIN);
    assert_eq!(
        route_names(&g),
        vec!["GET /empty", "GET /inline", "GET /items"],
        "one Route node per (file, verb, path)"
    );
    assert_eq!(
        handlers(&g, "GET /items"),
        vec![("list".into(), 5, "named", "call-arg".into())]
    );
}

/// Two registrations of one (verb, path) make one Route node with both
/// handlers: gin runs each group's chain, so both handlers serve it.
#[test]
fn test_route_gin_same_route_twice_one_node_two_handlers() {
    let src = r#"package main

import "github.com/gin-gonic/gin"

func first(c *gin.Context) {
	c.JSON(200, nil)
}

func second(c *gin.Context) {
	c.JSON(200, nil)
}

func main() {
	r := gin.Default()
	r.GET("/dup", first)
	r.GET("/dup", second)
}
"#;
    let g = build("main.go", src);
    assert_eq!(route_names(&g), vec!["GET /dup"]);
    assert_eq!(
        handlers(&g, "GET /dup"),
        vec![
            ("first".into(), 5, "named", "call-arg".into()),
            ("second".into(), 9, "named", "call-arg".into()),
        ]
    );
}
