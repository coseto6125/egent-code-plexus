//! NestJS `@Controller` prefix: a route method of a controller class is the controller prefixes joined to the method path by the one prefix rule (C1 `join_route_path`). The decorator pass and the TypeScript record both read the prefix, so one endpoint is one Route node.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use std::path::Path;

fn build(path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(TypeScriptProvider::new().unwrap()));
    let graph = pipeline
        .parse_file_raw(Path::new(path), src.as_bytes())
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(graph);
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

fn routes_of(controller: &str, methods: &str) -> Vec<String> {
    let src = format!(
        "import {{ Controller, Get, Post }} from '@nestjs/common';\n\n{controller}\nexport class C {{\n{methods}\n}}\n"
    );
    route_names(&build("src/c.controller.ts", &src))
}

const METHODS: &str = "  @Get('/list')\n  list() {}\n  @Post(':id')\n  create() {}";

#[test]
fn test_route_nestjs_router_prefix_composed() {
    assert_eq!(
        routes_of("@Controller('users')", METHODS),
        ["GET /users/list", "POST /users/:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_joint_has_one_slash() {
    assert_eq!(
        routes_of("@Controller('/users/')", METHODS),
        ["GET /users/list", "POST /users/:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_root_adds_nothing() {
    assert_eq!(
        routes_of("@Controller('/')", METHODS),
        ["GET /list", "POST /:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_absent_keeps_path() {
    assert_eq!(
        routes_of("@Controller()", METHODS),
        ["GET /list", "POST /:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_whitespace_only_adds_nothing() {
    assert_eq!(
        routes_of("@Controller('  ')", METHODS),
        ["GET /list", "POST /:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_object_path_composed() {
    assert_eq!(
        routes_of(
            "@Controller({ path: 'users', host: 'admin.example.com' })",
            METHODS
        ),
        ["GET /users/list", "POST /users/:id"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_list_gives_one_route_per_prefix() {
    assert_eq!(
        routes_of(
            "@Controller(['users', 'people'])",
            "  @Get('/list')\n  list() {}"
        ),
        ["GET /people/list", "GET /users/list"]
    );
}

#[test]
fn test_route_nestjs_router_prefix_empty_method_path_is_the_prefix() {
    assert_eq!(
        routes_of("@Controller('users')", "  @Get()\n  list() {}"),
        ["GET /users"]
    );
}
