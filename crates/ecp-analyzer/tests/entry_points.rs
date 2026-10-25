//! Integration tests for the cross-language entry-point scorer.
//!
//! Exercises the full builder pipeline end-to-end: a `LocalGraph`'s
//! `routes` / `framework_refs` / `nodes` flow through `score_entry_points`
//! and the builder emits `NodeKind::EntryPoint` marker nodes + `References`
//! edges into the final `ZeroCopyGraph`.
//!
//! Per spec §3 the file must cover at least:
//!   (a) Java `public static void main` detection
//!   (b) HTTP route from FastAPI/Spring/etc emits `EntryKind::HttpRoute`
//!   (c) Rust `fn main` detection
//!
//! Closes the ⚠️ Entry column for Java / Kotlin / C# / Go / Rust / Swift
//! / C / C++ / Dart in the README Language Matrix.

use ecp_analyzer::entry_points::{score_entry_points, EntryKind};
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, RawFrameworkRef, RawNode, RawRoute};
use ecp_core::graph::{NodeKind, RelType};

/// Minimal helper — every test below builds 1 LocalGraph; no need for
/// the full `mk_file` ceremony from the builder's internal tests.
fn mk_raw_node(name: &str, kind: NodeKind, decorators: Vec<String>) -> RawNode {
    RawNode {
        name: name.into(),
        kind,
        span: (0, 0, 0, 0),
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators,
        calls: vec![],
        field_reads: Vec::new(),
        owner_class: None,
        content_hash: 0,
    }
}

fn mk_local_graph(
    file_path: &str,
    nodes: Vec<RawNode>,
    routes: Vec<RawRoute>,
    framework_refs: Vec<RawFrameworkRef>,
) -> LocalGraph {
    LocalGraph {
        file_path: file_path.into(),
        content_hash: [0; 8],
        nodes,
        documents: vec![],
        imports: vec![],
        routes,
        framework_refs,
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        sql_refs: None,
        call_metas: vec![],
        raw_function_metas: vec![],
        export_aliases: None,
    }
}

/// Locate the EntryPoint marker node(s) in a built graph by inspecting
/// `node.kind`. Returns indices for downstream assertions.
fn entry_point_node_indices(graph: &ecp_core::graph::ZeroCopyGraph) -> Vec<usize> {
    graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n.kind, NodeKind::EntryPoint))
        .map(|(i, _)| i)
        .collect()
}

/// (c) Rust `fn main()` → builder must emit one `NodeKind::EntryPoint`
/// node pointing at the underlying `Function` via a `References` edge
/// at confidence 0.9.
#[test]
fn rust_fn_main_emits_entrypoint_marker() {
    let lg = mk_local_graph(
        "src/main.rs",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(
        entries.len(),
        1,
        "expected exactly 1 EntryPoint node, got {}",
        entries.len()
    );

    let entry_idx = entries[0];
    let entry_node = &graph.nodes[entry_idx];
    let name = entry_node.name.resolve(&graph.string_pool).to_string();
    assert!(
        name.contains("main"),
        "EntryPoint name should embed 'main', got {}",
        name
    );

    // Find the References edge from the marker to the handler.
    let entry_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source as usize == entry_idx && matches!(e.rel_type, RelType::References))
        .collect();
    assert_eq!(entry_edges.len(), 1, "expected 1 References edge");
    assert!(
        (entry_edges[0].confidence - 0.9).abs() < 1e-5,
        "confidence should be 0.9, got {}",
        entry_edges[0].confidence
    );
    let reason = entry_edges[0]
        .reason
        .resolve(&graph.string_pool)
        .to_string();
    assert!(
        reason.starts_with("main:"),
        "reason should start with main:, got {}",
        reason
    );
}

/// (a) Java `public static void main(String[] args)` — emitted by the
/// Java parser as a Method. Builder must emit the same EntryPoint
/// marker pattern as the Rust case.
#[test]
fn java_static_void_main_emits_entrypoint_marker() {
    let lg = mk_local_graph(
        "src/com/example/App.java",
        vec![
            mk_raw_node("App", NodeKind::Class, vec![]),
            mk_raw_node("main", NodeKind::Method, vec![]),
        ],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(
        entries.len(),
        1,
        "expected 1 EntryPoint, got {}",
        entries.len()
    );

    let entry_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source as usize == entries[0] && matches!(e.rel_type, RelType::References))
        .collect();
    assert_eq!(entry_edges.len(), 1);

    // Target should be the `main` Method node, not the App Class.
    let target_idx = entry_edges[0].target as usize;
    let target_node = &graph.nodes[target_idx];
    let target_name = target_node.name.resolve(&graph.string_pool).to_string();
    assert_eq!(target_name, "main");
    assert!(matches!(target_node.kind, NodeKind::Method));
}

/// (b) FastAPI `@app.get("/items")` → `RawRoute { handler: Some(...) }`.
/// Builder must emit an EntryPoint at score 1.0.
#[test]
fn fastapi_route_emits_entrypoint_at_score_1_0() {
    let lg = mk_local_graph(
        "app/main.py",
        vec![mk_raw_node("read_items", NodeKind::Function, vec![])],
        vec![RawRoute {
            method: "GET".into(),
            path: "/items".into(),
            handler: RawRoute::named_handler(Some("read_items")),
            span: (0, 0, 0, 0),
            scope: None,
        }],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(entries.len(), 1);

    let entry_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source as usize == entries[0] && matches!(e.rel_type, RelType::References))
        .collect();
    assert_eq!(entry_edges.len(), 1);
    assert!(
        (entry_edges[0].confidence - 1.0).abs() < 1e-5,
        "route confidence should be 1.0, got {}",
        entry_edges[0].confidence
    );
    let reason = entry_edges[0]
        .reason
        .resolve(&graph.string_pool)
        .to_string();
    assert!(
        reason.starts_with("route:"),
        "reason should encode route kind, got {}",
        reason
    );
    assert!(reason.contains("/items"));
}

/// Spring `@RestController` + `@GetMapping("/users")` — Java parser
/// emits the route as a decorator on a Method (no RawRoute). Builder
/// must still pick it up via decorator walking.
#[test]
fn spring_decorator_route_emits_entrypoint() {
    let lg = mk_local_graph(
        "src/main/java/UserController.java",
        vec![mk_raw_node(
            "getUsers",
            NodeKind::Method,
            vec!["@GetMapping(\"/users\")".into()],
        )],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(entries.len(), 1);

    let entry_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.source as usize == entries[0] && matches!(e.rel_type, RelType::References))
        .collect();
    assert_eq!(entry_edges.len(), 1);
    assert!((entry_edges[0].confidence - 1.0).abs() < 1e-5);
}

/// `method = {GET, POST}` names two verbs on one handler: the builder emits
/// one Route per verb, and the handler stays one entry point.
#[test]
fn test_build_spring_request_mapping_method_array_one_route_per_verb() {
    let lg = mk_local_graph(
        "src/main/java/UserController.java",
        vec![mk_raw_node(
            "users",
            NodeKind::Method,
            vec!["@RequestMapping(value = \"/users\", method = {RequestMethod.GET, RequestMethod.POST})".into()],
        )],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let mut routes: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.resolve(&graph.string_pool))
        .collect();
    routes.sort_unstable();
    assert_eq!(routes, vec!["GET /users", "POST /users"]);
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// Go `func main()` — same Function + "main" name as Rust. Smoke test
/// for the Go column flip; the scorer doesn't actually inspect language
/// at all, but the README cell claim is "Go Entry ✓" so we pin the
/// behavior with a Go-shaped fixture.
#[test]
fn go_func_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "cmd/server/main.go",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    let entries = entry_point_node_indices(&graph);
    assert_eq!(entries.len(), 1);
}

/// Stands for: Go's `package main` yields a Namespace node named `main`
/// beside `func main`; the namespace is no handler, so the entry point binds
/// the function instead of counting two candidates and binding none.
#[test]
fn test_build_go_package_main_beside_func_main_binds_function() {
    let mut package = mk_raw_node("main", NodeKind::Namespace, vec![]);
    package.span = (4, 0, 4, 12);
    let mut func = mk_raw_node("main", NodeKind::Function, vec![]);
    func.span = (23, 0, 34, 1);
    let lg = mk_local_graph("cmd/gosh/main.go", vec![package, func], vec![], vec![]);
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(entries.len(), 1, "one EntryPoint marker");
    let targets: Vec<NodeKind> = graph
        .edges
        .iter()
        .filter(|e| e.source as usize == entries[0] && matches!(e.rel_type, RelType::References))
        .map(|e| graph.nodes[e.target as usize].kind)
        .collect();
    assert_eq!(targets, vec![NodeKind::Function]);
}

/// Kotlin top-level `fun main()` — Function kind, name "main".
#[test]
fn kotlin_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "src/main/kotlin/App.kt",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// C# `static void Main` — Method kind, capital "Main".
#[test]
fn csharp_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "Program.cs",
        vec![mk_raw_node("Main", NodeKind::Method, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// Swift `@main` struct — decorator-style entry marker on a Class.
#[test]
fn swift_at_main_struct_emits_entrypoint() {
    let lg = mk_local_graph(
        "Sources/MyApp/MyApp.swift",
        vec![mk_raw_node("MyApp", NodeKind::Class, vec!["@main".into()])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// C `int main(int argc, char** argv)` — Function kind, "main" name.
#[test]
fn c_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "src/main.c",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// C++ `int main()` — same Function + "main" pattern.
#[test]
fn cpp_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "src/main.cpp",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// Dart `void main(List<String> args)` — Function kind, "main" name.
#[test]
fn dart_main_emits_entrypoint() {
    let lg = mk_local_graph(
        "bin/server.dart",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    let graph = builder.build();
    assert_eq!(entry_point_node_indices(&graph).len(), 1);
}

/// Framework decorator above the 0.8 floor → EntryKind::FrameworkRef
/// emitted at the original confidence (NestJS `@Controller`-style).
/// Pure scorer-level test (no builder) — verifies the public API surface.
#[test]
fn framework_ref_scorer_above_floor() {
    let fw = vec![RawFrameworkRef {
        source_name: "AppController".into(),
        target_name: "UserService".into(),
        confidence: 0.9,
        reason: "nestjs-controller".into(),
        span: (0, 0, 0, 0),
    }];
    let nodes = vec![mk_raw_node("AppController", NodeKind::Class, vec![])];
    let eps = score_entry_points(&[], &fw, &nodes);
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0].kind, EntryKind::FrameworkRef);
    assert!((eps[0].score - 0.9).abs() < 1e-6);
}

/// Multi-file build: each LocalGraph contributes its own scored entry
/// points; markers don't bleed across files. Locks in the per-file
/// scoring isolation contract.
#[test]
fn multi_file_entries_are_isolated_per_file() {
    let file_a = mk_local_graph(
        "src/main.rs",
        vec![mk_raw_node("main", NodeKind::Function, vec![])],
        vec![],
        vec![],
    );
    let file_b = mk_local_graph(
        "src/lib.rs",
        vec![mk_raw_node(
            "not_an_entry_point",
            NodeKind::Function,
            vec![],
        )],
        vec![],
        vec![],
    );
    let mut builder = GraphBuilder::new();
    builder.add_graph(file_a);
    builder.add_graph(file_b);
    let graph = builder.build();

    let entries = entry_point_node_indices(&graph);
    assert_eq!(entries.len(), 1, "only file_a has an entry point");
    // Resolve the actual file_idx for src/main.rs from the graph's files array.
    // Do not hardcode 0: build() sorts local_graphs by file_path for
    // determinism (inv-003), so src/lib.rs < src/main.rs lexicographically
    // and main.rs lands at index 1 in a sorted build.
    let main_rs_idx = graph
        .files
        .iter()
        .enumerate()
        .find(|(_, f)| f.path.resolve(&graph.string_pool).ends_with("main.rs"))
        .map(|(i, _)| i as u32)
        .expect("src/main.rs must be present in the built graph");
    let entry_file_idx = graph.nodes[entries[0]].file_idx;
    assert_eq!(
        entry_file_idx, main_rs_idx,
        "EntryPoint's file_idx must match src/main.rs, not src/lib.rs"
    );
}

/// A route of a Test or Reference file has no Route node (`pass1_5` skips
/// those files), so its handler is no entry point either. On HEAD the
/// bound handler of a NestJS `*.spec.ts` route became a `route@…` marker.
/// The same route at a production path is the control. One path per
/// mainstream language's test-file convention, plus one vendored path.
#[test]
fn test_build_route_in_test_file_no_entrypoint() {
    let entries_at = |path: &str| {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_local_graph(
            path,
            vec![mk_raw_node("read_items", NodeKind::Function, vec![])],
            vec![RawRoute {
                method: "GET".into(),
                path: "/items".into(),
                handler: RawRoute::named_handler(Some("read_items")),
                span: (0, 0, 0, 0),
                scope: None,
            }],
            vec![],
        ));
        entry_point_node_indices(&builder.build()).len()
    };
    assert_eq!(entries_at("app/main.py"), 1, "control: production route");
    for path in [
        "integration/hello-world/e2e/users.spec.ts",
        "test/app.router.js",
        "tests/test_main.py",
        "src/test/java/com/example/UserControllerTest.java",
        "src/test/kotlin/com/example/UserControllerTest.kt",
        "Tests/Api/UserControllerTests.cs",
        "gin_test.go",
        "tests/routes.rs",
        "tests/Feature/UserTest.php",
        "spec/requests/users_spec.rb",
        "Tests/AppTests/RouteTests.swift",
        "tests/test_server.c",
        "tests/server_test.cpp",
        "test/server_test.dart",
        "node_modules/express/lib/router.js",
    ] {
        assert_eq!(entries_at(path), 0, "{path}: no entry point");
    }
}

/// A decorator-declared route of a test file marks no entry point either:
/// the scorer reads the decorator itself, not a `RawRoute`, so on HEAD a
/// `@app.get` handler in `tests/` still became a `route@` marker.
#[test]
fn test_build_decorator_route_in_test_file_no_entrypoint() {
    let entries_at = |path: &str| {
        let mut builder = GraphBuilder::new();
        builder.add_graph(mk_local_graph(
            path,
            vec![mk_raw_node(
                "read_items",
                NodeKind::Function,
                vec!["@app.get(\"/items\")".into()],
            )],
            vec![],
            vec![],
        ));
        entry_point_node_indices(&builder.build()).len()
    };
    assert_eq!(entries_at("app/main.py"), 1, "control: production route");
    assert_eq!(entries_at("tests/test_main.py"), 0);
    assert_eq!(entries_at("integration/hello-world/e2e/users.spec.ts"), 0);
}
