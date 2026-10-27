//! Spring class-level `@RequestMapping` prefix: a mapped method of the class is the class paths joined to the method path by the one prefix rule (C1 `join_route_path`).

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::graph::{NodeKind, ZeroCopyGraph};
use std::path::Path;

fn build(path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(JavaProvider::new().unwrap()));
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

fn routes_of(class_annotation: &str, methods: &str) -> Vec<String> {
    let src = format!(
        "import org.springframework.web.bind.annotation.*;\n\n{class_annotation}\npublic class C {{\n{methods}\n}}\n"
    );
    route_names(&build("src/C.java", &src))
}

const METHODS: &str = "  @GetMapping(\"/list\")\n  public void list() {}\n  @PostMapping(\"/new\")\n  public void create() {}";

#[test]
fn test_route_spring_router_prefix_composed() {
    assert_eq!(
        routes_of("@RestController\n@RequestMapping(\"/api\")", METHODS),
        ["GET /api/list", "POST /api/new"]
    );
}

#[test]
fn test_route_spring_router_prefix_joint_has_one_slash() {
    assert_eq!(
        routes_of("@RestController\n@RequestMapping(\"/api/\")", METHODS),
        ["GET /api/list", "POST /api/new"]
    );
}

#[test]
fn test_route_spring_router_prefix_root_adds_nothing() {
    assert_eq!(
        routes_of("@RestController\n@RequestMapping(\"/\")", METHODS),
        ["GET /list", "POST /new"]
    );
}

#[test]
fn test_route_spring_router_prefix_absent_keeps_path() {
    assert_eq!(
        routes_of("@RestController", METHODS),
        ["GET /list", "POST /new"]
    );
}

#[test]
fn test_route_spring_router_prefix_empty_adds_nothing() {
    assert_eq!(
        routes_of("@RestController\n@RequestMapping(\"\")", METHODS),
        ["GET /list", "POST /new"]
    );
}

#[test]
fn test_route_spring_router_prefix_list_gives_one_route_per_prefix() {
    assert_eq!(
        routes_of(
            "@RestController\n@RequestMapping(value = {\"/a\", \"/b\"})",
            "  @GetMapping(\"/list\")\n  public void list() {}"
        ),
        ["GET /a/list", "GET /b/list"]
    );
}

#[test]
fn test_route_spring_controller_bean_name_is_no_prefix() {
    assert_eq!(
        routes_of("@Controller(\"/beanName\")", METHODS),
        ["GET /list", "POST /new"]
    );
}
