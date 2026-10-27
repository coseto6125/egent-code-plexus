//! Go router-group prefix: a route registered on a `Group` variable is the group prefixes joined to the route path by the one prefix rule (C1 `join_route_path`), one `/` at each joint. The route name is the `RouteKey` name and its uid follows it.

use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use std::path::Path;

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

fn routes_of(body: &str) -> Vec<String> {
    let src = format!(
        "package main\n\nimport \"github.com/gin-gonic/gin\"\n\nfunc main() {{\n\tr := gin.Default()\n{body}\n}}\n"
    );
    route_names(&build("main.go", &src))
}

#[test]
fn test_route_gin_router_prefix_composed() {
    let names = routes_of(
        "\tv1 := r.Group(\"/v1\")\n\tv1.GET(\"/users\", h)\n\tu := v1.Group(\"/admin/\")\n\tu.POST(\"/new\", h)",
    );
    assert_eq!(names, ["GET /v1/users", "POST /v1/admin/new"]);
}

#[test]
fn test_route_gin_router_prefix_joint_has_one_slash() {
    let names = routes_of("\tg := r.Group(\"/api/\")\n\tg.GET(\"/users\", h)");
    assert_eq!(names, ["GET /api/users"]);
}

#[test]
fn test_route_gin_router_prefix_root_adds_nothing() {
    let names = routes_of("\tg := r.Group(\"/\")\n\tg.GET(\"/users\", h)");
    assert_eq!(names, ["GET /users"]);
}

#[test]
fn test_route_gin_router_prefix_empty_adds_nothing() {
    let names = routes_of("\tg := r.Group(\"\")\n\tg.GET(\"/users\", h)");
    assert_eq!(names, ["GET /users"]);
}

#[test]
fn test_route_gin_router_prefix_whitespace_only_adds_nothing() {
    let names = routes_of("\tg := r.Group(\"  \")\n\tg.GET(\"/users\", h)");
    assert_eq!(names, ["GET /users"]);
}

#[test]
fn test_route_gin_router_prefix_absent_keeps_path() {
    let names = routes_of("\tr.GET(\"/users\", h)");
    assert_eq!(names, ["GET /users"]);
}

#[test]
fn test_route_gin_router_prefix_unknown_group_argument_adds_nothing() {
    let names = routes_of("\tg := r.Group(base)\n\tg.GET(\"/users\", h)");
    assert_eq!(names, ["GET /users"]);
}

#[test]
fn test_route_gin_router_prefix_groups_stay_apart() {
    let names = routes_of(
        "\ta := r.Group(\"/a\")\n\tb := r.Group(\"/b\")\n\ta.GET(\"/x\", h)\n\tb.GET(\"/x\", h)",
    );
    assert_eq!(names, ["GET /a/x", "GET /b/x"]);
}

#[test]
fn test_route_gin_verb_substring_method_is_no_route() {
    let names = routes_of("\tr.GetTarget(\"/users\", h)\n\tr.Outputs(\"/users\", h)");
    assert_eq!(names, Vec::<String>::new());
}
