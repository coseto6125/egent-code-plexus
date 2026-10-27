//! Java route identity: one Route node per (file, verb, path) (C1 `RouteKey`),
//! and the handler edge starts at the annotated method (C4 `RawRoute.handler`).

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::route_detector::{BS_KIND_ROUTE_HANDLER_ABSENT, BS_KIND_ROUTE_UNBOUND_HANDLER};
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::identity::HandlerRef;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// Parse through the pipeline, which binds record endpoints, then build.
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

const SPRING: &str = r#"package demo;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class UserController {
    @GetMapping("/users")
    public String find() {
        return "all";
    }

    @GetMapping("/users/{id}")
    public String find(String id) {
        return id;
    }

    @PostMapping("/users")
    public String create() {
        return "new";
    }
}
"#;

/// Each mapping annotation makes one Route node, and its edge starts at
/// the method it annotates. The two `find` overloads keep their own routes.
/// Regression guard: Spring has one route producer, so HEAD passes too.
#[test]
fn test_route_java_spring_one_node_per_route_binds_annotated_method() {
    let g = build("src/main/java/demo/UserController.java", SPRING);
    assert_eq!(
        route_names(&g),
        vec!["GET /users", "GET /users/{id}", "POST /users"],
        "one Route node per (file, verb, path)"
    );
    let all = handlers(&g, "GET /users");
    let one = handlers(&g, "GET /users/{id}");
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(one.len(), 1, "{one:?}");
    assert_eq!(
        (all[0].0.as_str(), all[0].3.as_str()),
        ("find", "decorator")
    );
    assert_eq!(
        (one[0].0.as_str(), one[0].3.as_str()),
        ("find", "decorator")
    );
    assert!(
        all[0].1 < one[0].1,
        "each overload handles its own route: {all:?} {one:?}"
    );
    let create = handlers(&g, "POST /users");
    assert_eq!(create.len(), 1, "{create:?}");
    assert_eq!(create[0].0, "create");
    assert_eq!(handler_spot_kinds(&g), Vec::<String>::new());
}

/// A Spring mapping on an HTTP client interface (`@FeignClient`,
/// `@HttpExchange`) describes a request the application sends, so it
/// declares no route. A mapping on a plain class still routes: a
/// controller in another file can extend it, and nothing in this file
/// proves otherwise.
#[test]
fn test_route_java_spring_mapping_on_client_interface_no_route() {
    let src = r#"package com.example;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class UserController {
    @GetMapping("/users")
    public String list() { return "all"; }
}

class BaseController {
    @GetMapping("/x")
    public String inherited() { return "x"; }
}

@FeignClient(name = "users")
interface UserClient {
    @GetMapping("/remote")
    String fetch();
}

@HttpExchange("/api")
interface OrderClient {
    @GetMapping("/orders")
    String orders();
}
"#;
    let g = build("src/main/java/com/example/UserController.java", src);
    assert_eq!(route_names(&g), vec!["GET /users", "GET /x"]);
}

/// Spring serves the mappings an interface declares for the controller
/// that implements it (openapi-generator's `PetApi` with `@GetMapping`
/// default methods), and a composed stereotype such as `@ApiController`
/// marks a controller. On HEAD a mapping routed only on a class carrying
/// `@Controller` or `@RestController`, so both routes disappeared.
#[test]
fn test_route_java_spring_mapping_on_interface_and_composed_stereotype_routes() {
    let src = r#"package com.example;

import org.springframework.web.bind.annotation.GetMapping;

public interface PetApi {
    @GetMapping("/pets")
    default String listPets() { return "pets"; }
}

@ApiController
class StoreController {
    @GetMapping("/store")
    public String inventory() { return "store"; }
}
"#;
    let g = build("src/main/java/com/example/PetApi.java", src);
    assert_eq!(route_names(&g), vec!["GET /pets", "GET /store"]);
    let pets = handlers(&g, "GET /pets");
    assert_eq!(pets.len(), 1, "{pets:?}");
    assert_eq!(pets[0].0, "listPets");
    let store = handlers(&g, "GET /store");
    assert_eq!(store.len(), 1, "{store:?}");
    assert_eq!(store[0].0, "inventory");
}
