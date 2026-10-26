//! FastAPI router prefix: a route registered on an `APIRouter(prefix=...)` variable, or on a router that the same file includes with `include_router(..., prefix=...)`, is the prefixes joined to the route path by the one prefix rule (C1 `join_route_path`). The parser record carries the composed path and the decorator pass drops its own `GET /x`, so one handler is one Route node. A router that another file includes keeps its own path.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build_files(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        let graph = pipeline
            .parse_file_raw(Path::new(path), src.as_bytes())
            .unwrap();
        builder.add_graph(graph);
    }
    builder.build()
}

/// Every Route node's name, sorted, duplicates kept.
fn route_names(g: &ZeroCopyGraph) -> Vec<String> {
    let mut names: Vec<String> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.resolve(&g.string_pool).to_string())
        .collect();
    names.sort_unstable();
    names
}

/// The routes of one file that defines `router = APIRouter(<args>)` with a
/// handler `read_x` on `@router.get("/x")`, followed by `tail`.
fn routes_of(router_args: &str, tail: &str) -> Vec<String> {
    let src = format!(
        "from fastapi import APIRouter, FastAPI\n\napp = FastAPI()\nrouter = APIRouter({router_args})\n\n@router.get(\"/x\")\ndef read_x():\n    return {{}}\n\n{tail}\n"
    );
    route_names(&build_files(&[("app/main.py", &src)]))
}

#[test]
fn test_route_fastapi_router_prefix_composed() {
    assert_eq!(
        routes_of("prefix=\"/items\"", ""),
        ["GET /items/x"],
        "one handler is one Route, at the composed path, and no GET /x"
    );
}

#[test]
fn test_route_fastapi_include_router_prefix_composed() {
    assert_eq!(
        routes_of("", "app.include_router(router, prefix=\"/v1\")"),
        ["GET /v1/x"]
    );
}

#[test]
fn test_route_fastapi_router_and_include_prefix_compose_in_order() {
    assert_eq!(
        routes_of(
            "prefix=\"/items\"",
            "app.include_router(router, prefix=\"/v1\")"
        ),
        ["GET /v1/items/x"]
    );
}

#[test]
fn test_route_fastapi_nested_include_prefixes_compose_outermost_first() {
    let src = "from fastapi import APIRouter, FastAPI\n\napp = FastAPI()\nouter = APIRouter(prefix=\"/o\")\ninner = APIRouter(prefix=\"/i\")\n\n@inner.get(\"/x\")\ndef read_x():\n    return {}\n\nouter.include_router(inner, prefix=\"/mid\")\napp.include_router(outer, prefix=\"/v1\")\n";
    assert_eq!(
        route_names(&build_files(&[("app/main.py", src)])),
        ["GET /v1/o/mid/i/x"]
    );
}

#[test]
fn test_route_fastapi_cross_file_include_router_keeps_own_path_one_route() {
    let routes = "from fastapi import APIRouter\n\nrouter = APIRouter()\n\n@router.get(\"/x\")\ndef read_x():\n    return {}\n";
    let main = "from fastapi import FastAPI\nfrom app.routes import router\n\napp = FastAPI()\napp.include_router(router, prefix=\"/v1\")\n";
    assert_eq!(
        route_names(&build_files(&[
            ("app/routes.py", routes),
            ("app/main.py", main)
        ])),
        ["GET /x"],
        "a cross-file include adds no prefix, and the handler has one Route"
    );
}

#[test]
fn test_route_fastapi_two_includes_of_one_router_give_one_route_each() {
    assert_eq!(
        routes_of(
            "",
            "app.include_router(router, prefix=\"/a\")\napp.include_router(router, prefix=\"/b\")"
        ),
        ["GET /a/x", "GET /b/x"]
    );
}

#[test]
fn test_route_fastapi_empty_prefix_adds_nothing() {
    assert_eq!(routes_of("prefix=\"\"", ""), ["GET /x"]);
}

#[test]
fn test_route_fastapi_root_prefix_adds_nothing() {
    assert_eq!(routes_of("prefix=\"/\"", ""), ["GET /x"]);
}

#[test]
fn test_route_fastapi_prefix_trailing_slash_joint_has_one_slash() {
    assert_eq!(routes_of("prefix=\"/items/\"", ""), ["GET /items/x"]);
}

#[test]
fn test_route_fastapi_whitespace_only_prefix_adds_nothing() {
    assert_eq!(routes_of("prefix=\"   \"", ""), ["GET /x"]);
    assert_eq!(
        routes_of("", "app.include_router(router, prefix=\"  \")"),
        ["GET /x"]
    );
}

#[test]
fn test_route_fastapi_non_literal_prefix_adds_nothing() {
    assert_eq!(routes_of("prefix=PREFIX", ""), ["GET /x"]);
    assert_eq!(
        routes_of("", "app.include_router(router, prefix=PREFIX)"),
        ["GET /x"]
    );
}

#[test]
fn test_route_fastapi_list_for_scalar_prefix_adds_nothing() {
    assert_eq!(routes_of("prefix=[\"/a\", \"/b\"]", ""), ["GET /x"]);
    assert_eq!(
        routes_of("", "app.include_router(router, prefix=[\"/a\", \"/b\"])"),
        ["GET /x"]
    );
}

#[test]
fn test_route_fastapi_interpolated_prefix_adds_nothing() {
    assert_eq!(
        routes_of("prefix=f\"/{version}/items\"", ""),
        ["GET /x"],
        "an f-string prefix is not a literal"
    );
}

#[test]
fn test_route_fastapi_empty_file_gives_no_route() {
    assert!(route_names(&build_files(&[("app/main.py", "")])).is_empty());
}

#[test]
fn test_route_fastapi_composed_route_keeps_its_handler_edge() {
    let src = "from fastapi import APIRouter\n\nrouter = APIRouter(prefix=\"/items\")\n\n@router.get(\"/x\")\ndef read_x():\n    return {}\n";
    let g = build_files(&[("app/main.py", src)]);
    let handles: Vec<(String, String)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::HandlesRoute)
        .map(|e| {
            (
                g.nodes[e.source as usize]
                    .name
                    .resolve(&g.string_pool)
                    .to_string(),
                g.nodes[e.target as usize]
                    .name
                    .resolve(&g.string_pool)
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        handles,
        [("read_x".to_string(), "GET /items/x".to_string())],
        "the decorator route is dropped, so the handler edge points at the composed Route only"
    );
}

#[test]
fn test_route_fastapi_includes_past_the_prefix_cap_keep_the_route() {
    // Stands for: the cap bounds the composed paths (16) and never drops the
    // endpoint: the route stays reachable at its own path as well.
    let tail: String = (0..20)
        .map(|i| format!("app.include_router(router, prefix=\"/v{i}\")\n"))
        .collect();
    let names = routes_of("", &tail);
    assert_eq!(
        names.len(),
        17,
        "16 composed paths and the route's own: {names:?}"
    );
    assert!(names.iter().any(|n| n == "GET /x"), "{names:?}");
}
