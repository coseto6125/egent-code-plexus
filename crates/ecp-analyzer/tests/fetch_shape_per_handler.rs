//! Response shape per handler (flow F8): the keys of a Route's response shape come from the span of the handler that serves it, in the handler's own file, never from the text of the whole route file. A file with two handlers gives each Route its own keys, so a client that reads a field only the other handler returns is a drift. A Laravel controller action that Pass 1.95 binds gives its route the keys of the action's body. A route whose handler body has no readable span reads its route file only when that file holds one handler, whose body is in the file; in a file of several handlers it has no shape, never the union of the file.

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::analyzer::types::{LocalGraph, RawNode, RawRoute};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const API: &str = "const express = require('express');\nconst router = express.Router();\nfunction listUsers(req, res) { res.json({ id: 1, name: 'a' }); }\nfunction listOrders(req, res) { res.status(404).json({ reason: 'none' }); res.json({ total: 5 }); }\nrouter.get('/users', listUsers);\nrouter.get('/orders', listOrders);\n";

fn build(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().unwrap();
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(JavaScriptProvider::new().unwrap()));
    pipeline.register_provider(Box::new(PhpProvider::new().unwrap()));
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    for (path, src) in files {
        let abs = tmp.path().join(path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, src).unwrap();
        builder.add_graph(
            pipeline
                .parse_file_raw(Path::new(path), src.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

/// The `(response keys, error keys)` of the Route called `name`, `None` for
/// a Route with no shape.
fn shape_of(g: &ZeroCopyGraph, name: &str) -> Option<(Vec<String>, Vec<String>)> {
    let route = g
        .nodes
        .iter()
        .position(|n| n.kind == NodeKind::Route && n.name.resolve(&g.string_pool) == name)?;
    let shape = g
        .route_shapes
        .iter()
        .find(|s| s.node_idx as usize == route)?;
    let keys = |refs: &[ecp_core::pool::StrRef]| {
        refs.iter()
            .map(|r| r.resolve(&g.string_pool).to_string())
            .collect::<Vec<_>>()
    };
    Some((keys(&shape.response_keys), keys(&shape.error_keys)))
}

#[test]
fn test_fetch_shape_two_handlers_one_file_compare_separately() {
    let client = "async function load() { const data = await (await fetch('/users')).json(); return data.total; }\n";
    let g = build(&[("api.js", API), ("client.js", client)]);

    let (users, _) = shape_of(&g, "GET /users").expect("GET /users has a shape");
    assert_eq!(
        users,
        ["id", "name"],
        "the users shape holds the keys of listUsers only"
    );
    assert!(
        !users.contains(&"total".to_string()),
        "the client reads `total`, which only listOrders returns: a drift against /users"
    );

    let (orders, orders_err) = shape_of(&g, "GET /orders").expect("GET /orders has a shape");
    assert_eq!(orders, ["total"]);
    assert_eq!(
        orders_err,
        ["reason"],
        "the 404 body of listOrders is an error key of /orders only"
    );
    let (_, users_err) = shape_of(&g, "GET /users").unwrap();
    assert!(users_err.is_empty(), "no error key leaks across handlers");

    let reads_total = g.edges.iter().any(|e| {
        e.rel_type == RelType::Fetches
            && g.nodes[e.target as usize].name.resolve(&g.string_pool) == "GET /users"
            && e.reason.resolve(&g.string_pool).contains("total")
    });
    assert!(
        reads_total,
        "the Fetches edge to GET /users records the key `total` the client reads"
    );
}

#[test]
fn test_fetch_shape_handler_without_other_json_calls_gives_no_file_union() {
    let src = "function a(req, res) { res.json({ one: 1 }); }\nfunction b(req, res) { res.send('x'); }\nconst router = require('express').Router();\nrouter.get('/a', a);\nrouter.get('/b', b);\n";
    let g = build(&[("api.js", src)]);
    assert_eq!(shape_of(&g, "GET /a").unwrap().0, ["one"]);
    assert_eq!(
        shape_of(&g, "GET /b"),
        None,
        "b returns no JSON, and the `one` of a is not its shape"
    );
}

/// One route handler: `(function name, route path, function span)`.
type Handler<'a> = (&'a str, &'a str, (u32, u32, u32, u32));

fn graph_with_span(span: (u32, u32, u32, u32)) -> LocalGraph {
    graph_with_handlers(&[("getUsers", "/users", span)])
}

/// `api.js` with one function node per `(name, path, span)`, and a route
/// `GET <path>` served by it. Each route call sits on its own row past the
/// nodes, so the handler binds by name.
fn graph_with_handlers(handlers: &[Handler]) -> LocalGraph {
    let site = |row: u32| match handlers.len() {
        1 => (0, 0, 0, 0),
        _ => (100 + row, 0, 100 + row, 10),
    };
    LocalGraph {
        file_path: "api.js".into(),
        content_hash: [0; 8],
        nodes: handlers
            .iter()
            .map(|(name, _, span)| RawNode {
                name: (*name).into(),
                kind: NodeKind::Function,
                span: *span,
                is_exported: false,
                heritage: vec![],
                type_annotation: None,
                decorators: vec![],
                calls: vec![],
                field_reads: Vec::new(),
                owner_class: None,
                content_hash: 0,
            })
            .collect(),
        documents: vec![],
        imports: vec![],
        routes: handlers
            .iter()
            .zip(0u32..)
            .map(|((name, path, _), row)| RawRoute {
                method: "get".into(),
                path: (*path).into(),
                handler: RawRoute::named_handler(Some(*name)),
                span: site(row),
                scope: None,
            })
            .collect(),
        route_receivers: None,
        framework_refs: vec![],
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

fn build_file_with_graph(src: &str, graph: LocalGraph) -> ZeroCopyGraph {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("api.js"), src).unwrap();
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(graph);
    builder.build()
}

const ONE_LINE: &str = "function getUsers(req, res) { res.json({ id, name }); }";

#[test]
fn test_fetch_shape_one_handler_with_no_body_span_reads_the_file() {
    let g = build_file_with_graph(ONE_LINE, graph_with_span((0, 0, 0, 0)));
    assert_eq!(
        shape_of(&g, "GET /users").unwrap().0,
        ["id", "name"],
        "the file holds one handler, so its text is that handler's shape"
    );
}

const TWO_LINES: &str = "function getUsers(req, res) { res.json({ id, name }); }\nfunction getOrders(req, res) { res.json({ total }); }\n";

#[test]
fn test_fetch_shape_two_handlers_with_no_body_span_are_unknown_not_file_union() {
    for span in [(0, 0, 0, 0), (40, 0, 41, 3)] {
        let g = build_file_with_graph(
            TWO_LINES,
            graph_with_handlers(&[
                ("getUsers", "/users", span),
                ("getOrders", "/orders", (1, 0, 1, 53)),
            ]),
        );
        assert_eq!(
            shape_of(&g, "GET /users"),
            None,
            "{span:?} has no text, and the file of two handlers is no handler's shape"
        );
        assert_eq!(
            shape_of(&g, "GET /orders").unwrap().0,
            ["total"],
            "the readable handler keeps its own keys"
        );
    }
}

#[test]
fn test_fetch_shape_handler_span_covering_the_body_reads_its_keys() {
    let g = build_file_with_graph(ONE_LINE, graph_with_span((0, 0, 0, ONE_LINE.len() as u32)));
    assert_eq!(shape_of(&g, "GET /users").unwrap().0, ["id", "name"]);
}

#[test]
fn test_fetch_shape_one_handler_with_span_outside_the_file_reads_the_file() {
    let g = build_file_with_graph(ONE_LINE, graph_with_span((40, 0, 41, 3)));
    assert_eq!(shape_of(&g, "GET /users").unwrap().0, ["id", "name"]);
}

#[test]
fn test_fetch_shape_empty_file_is_unknown() {
    let g = build_file_with_graph("", graph_with_span((0, 0, 0, 12)));
    assert_eq!(shape_of(&g, "GET /users"), None);
}

#[test]
fn test_fetch_shape_whitespace_only_file_is_unknown() {
    let g = build_file_with_graph("   \n\t\n", graph_with_span((0, 0, 1, 1)));
    assert_eq!(shape_of(&g, "GET /users"), None);
}

#[test]
fn test_fetch_shape_file_with_no_routes_gives_no_shape() {
    let g = build(&[("api.js", "function f(req, res) { res.json({ id: 1 }); }\n")]);
    assert!(
        g.route_shapes.is_empty(),
        "a handler body that serves no route is no route's shape"
    );
}

#[test]
fn test_fetch_shape_router_mounted_twice_both_routes_read_the_handler() {
    let src = "const express = require('express');\nconst app = express();\nconst router = express.Router();\nfunction h(req, res) { res.json({ id: 1 }); }\nrouter.get('/x', h);\napp.use('/a', router);\napp.use('/b', router);\n";
    let g = build(&[("api.js", src)]);
    assert_eq!(shape_of(&g, "GET /a/x").unwrap().0, ["id"]);
    assert_eq!(shape_of(&g, "GET /b/x").unwrap().0, ["id"]);
}

const USER_CONTROLLER: &str = "<?php\nnamespace App\\Http\\Controllers;\n\nclass UserController extends Controller\n{\n    public function show() { echo json_encode(['id' => 1]); }\n    public function orders() { echo json_encode(['total' => 2]); }\n}\n";

#[test]
fn test_fetch_shape_controller_action_bound_later_reads_the_action_body() {
    let routes = "<?php\nuse Illuminate\\Support\\Facades\\Route;\nRoute::get('/users', 'UserController@show');\nRoute::get('/orders', 'UserController@orders');\n";
    let g = build(&[
        ("routes/api.php", routes),
        ("app/Http/Controllers/UserController.php", USER_CONTROLLER),
    ]);
    assert_eq!(
        shape_of(&g, "GET /users").unwrap().0,
        ["id"],
        "Pass 1.95 binds `show`, and the route reads its body in the controller file"
    );
    assert_eq!(shape_of(&g, "GET /orders").unwrap().0, ["total"]);
}

#[test]
fn test_fetch_shape_unbound_controller_action_is_unknown() {
    let routes = "<?php\nuse Illuminate\\Support\\Facades\\Route;\nRoute::get('/users', 'MissingController@show');\necho json_encode(['stray' => 1]);\n";
    let g = build(&[("routes/api.php", routes)]);
    assert_eq!(
        shape_of(&g, "GET /users"),
        None,
        "the action's body is in no file of the build; the route file's text is not its shape"
    );
}
