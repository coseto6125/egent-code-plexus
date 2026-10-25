//! Express mount: `app.use("/api", router)` prefixes the routes of the mounted router when the router binding resolves, in the same file or through an import the build resolves. A mount that does not resolve leaves the route at its own path. One handler is one Route node in every case, and a router mounted twice gives one Route per mount. The prefix goes through the one prefix rule (C1 `join_route_path`).

use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build_files(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(JavaScriptProvider::new().unwrap()));
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

/// One file: a router with `GET /x`, then `tail`.
fn routes_of(tail: &str) -> Vec<String> {
    let src = format!(
        "const express = require('express');\nconst app = express();\nconst router = express.Router();\nfunction h(req, res) {{ res.json({{}}); }}\nrouter.get('/x', h);\n{tail}\n"
    );
    route_names(&build_files(&[("src/app.js", &src)]))
}

const USERS_ROUTER: &str = "const express = require('express');\nconst router = express.Router();\nfunction list(req, res) { res.json({}); }\nrouter.get('/list', list);\nmodule.exports = router;\n";

#[test]
fn test_route_express_mount_same_file_composed() {
    // Stands for: the composed route of the handler. The `USE` row is the
    // mount itself, a Route since before P8e (`route_detector.rs`
    // `detect_from_call_accepts_express_use_mount_point`).
    assert_eq!(
        routes_of("app.use('/api', router);"),
        ["GET /api/x", "USE /api"]
    );
}

#[test]
fn test_route_express_mount_imported_router_composed() {
    // Stands for: the composed route of the handler. The `USE` row is the
    // mount itself, a Route since before P8e (`route_detector.rs`
    // `detect_from_call_accepts_express_use_mount_point`).
    let app = "const express = require('express');\nconst users = require('./routes/users');\nconst app = express();\napp.use('/api', users);\n";
    assert_eq!(
        route_names(&build_files(&[
            ("src/routes/users.js", USERS_ROUTER),
            ("src/app.js", app)
        ])),
        ["GET /api/list", "USE /api"],
        "the mount composes onto the router of another file, and its handler has one Route"
    );
}

#[test]
fn test_route_express_mount_es_import_composed() {
    // Stands for: the composed route of the handler. The `USE` row is the
    // mount itself, a Route since before P8e (`route_detector.rs`
    // `detect_from_call_accepts_express_use_mount_point`).
    let app = "import express from 'express';\nimport users from './routes/users';\nconst app = express();\napp.use('/api', users);\n";
    assert_eq!(
        route_names(&build_files(&[
            ("src/routes/users.js", USERS_ROUTER),
            ("src/app.js", app)
        ])),
        ["GET /api/list", "USE /api"]
    );
}

#[test]
fn test_route_express_nested_mounts_compose_outermost_first() {
    // Stands for: the handler route composed outermost mount first. The two
    // `USE` rows are the mounts themselves; the inner mount sits on `outer`,
    // so it takes the `/v1` of `outer` too.
    let app = "const express = require('express');\nconst app = express();\nconst outer = express.Router();\nconst inner = express.Router();\nfunction h(req, res) { res.json({}); }\ninner.get('/x', h);\nouter.use('/mid', inner);\napp.use('/v1', outer);\n";
    assert_eq!(
        route_names(&build_files(&[("src/app.js", app)])),
        ["GET /v1/mid/x", "USE /v1", "USE /v1/mid"]
    );
}

#[test]
fn test_route_express_unresolved_mount_keeps_own_path_one_route() {
    // Stands for: the handler route at its own path, once. The `USE` row is
    // the mount itself, which keeps its path whether or not it resolves.
    // `mystery` binds no router of the file and no import.
    assert_eq!(
        routes_of("app.use('/api', mystery);"),
        ["GET /x", "USE /api"]
    );
    // The import names a module that is not in the build.
    let app = "const express = require('express');\nconst users = require('./routes/missing');\nconst app = express();\napp.use('/api', users);\n";
    assert_eq!(
        route_names(&build_files(&[
            ("src/routes/users.js", USERS_ROUTER),
            ("src/app.js", app)
        ])),
        ["GET /list", "USE /api"],
        "an import that resolves to no file leaves the route at its own path"
    );
}

#[test]
fn test_route_express_mount_of_a_function_adds_no_prefix() {
    // Stands for: a middleware function mounts no router, so `GET /x` keeps
    // its path. The `USE` row is the mount itself.
    assert_eq!(
        routes_of("app.use('/api', function (req, res, next) { next(); });"),
        ["GET /x", "USE /api"]
    );
}

#[test]
fn test_route_express_two_mounts_of_one_router_give_one_route_each() {
    // Stands for: one handler route per mount. The `USE` rows are the two
    // mounts themselves.
    assert_eq!(
        routes_of("app.use('/a', router);\napp.use('/b', router);"),
        ["GET /a/x", "GET /b/x", "USE /a", "USE /b"]
    );
}

#[test]
fn test_route_express_mount_root_adds_nothing() {
    // Stands for: the root mount adds no segment to `GET /x`. The `USE /`
    // row is the mount itself.
    assert_eq!(routes_of("app.use('/', router);"), ["GET /x", "USE /"]);
}

#[test]
fn test_route_express_mount_trailing_slash_joint_has_one_slash() {
    // Stands for: one slash at the joint of `/api/` and `/x`. The `USE` row
    // is the mount itself, at the path its call writes.
    assert_eq!(
        routes_of("app.use('/api/', router);"),
        ["GET /api/x", "USE /api/"]
    );
}

#[test]
fn test_route_express_empty_and_whitespace_mount_path_add_nothing() {
    assert_eq!(routes_of("app.use('', router);"), ["GET /x"]);
    assert_eq!(routes_of("app.use('   ', router);"), ["GET /x"]);
}

#[test]
fn test_route_express_non_literal_and_list_mount_path_add_nothing() {
    assert_eq!(routes_of("app.use(PREFIX, router);"), ["GET /x"]);
    assert_eq!(routes_of("app.use(['/a', '/b'], router);"), ["GET /x"]);
}

#[test]
fn test_route_express_mount_in_test_file_adds_no_prefix() {
    let src = "const express = require('express');\nconst app = express();\nconst router = express.Router();\nfunction h(req, res) { res.json({}); }\nrouter.get('/x', h);\napp.use('/api', router);\n";
    let g = build_files(&[("src/app.js", &src.replace("app.use('/api', router);\n", ""))]);
    let mounted_in_test = build_files(&[
        ("src/app.js", &src.replace("app.use('/api', router);\n", "")),
        (
            "src/app.test.js",
            "const app = require('express')();\nconst router = require('./app');\napp.use('/api', router);\n",
        ),
    ]);
    assert_eq!(route_names(&g), ["GET /x"]);
    assert_eq!(
        route_names(&mounted_in_test),
        ["GET /x"],
        "a mount in a test file does not prefix a production route"
    );
}

#[test]
fn test_route_express_empty_file_gives_no_route() {
    assert!(route_names(&build_files(&[("src/app.js", "")])).is_empty());
}

#[test]
fn test_route_express_mounted_route_keeps_its_handler_edge() {
    let src = "const express = require('express');\nconst app = express();\nconst router = express.Router();\nfunction h(req, res) { res.json({}); }\nrouter.get('/x', h);\napp.use('/api', router);\n";
    let g = build_files(&[("src/app.js", src)]);
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
    assert_eq!(handles, [("h".to_string(), "GET /api/x".to_string())]);
}

#[test]
fn test_route_express_mount_named_import_of_a_non_router_composes_nothing() {
    // Stands for: a route keeps its own path unless the mounted binding is
    // proved to be its router. `auth` is a middleware exported next to the
    // file's one router; mounting it must not give the router's route `/api`.
    let feature = "const express = require('express');\nconst router = express.Router();\nfunction priv(req, res) { res.send('x'); }\nrouter.get('/private', priv);\nfunction auth(req, res, next) { next(); }\nmodule.exports = { auth, router };\n";
    let app = "import express from 'express';\nimport { auth } from './feature';\nconst app = express();\napp.use('/api', auth);\n";
    assert_eq!(
        route_names(&build_files(&[
            ("src/feature.js", feature),
            ("src/app.js", app)
        ])),
        ["GET /private", "USE /api"],
        "a named import that matches no router mounts no router"
    );
}

#[test]
fn test_route_express_mount_named_import_of_the_router_still_composes() {
    // Stands for: the by-name match is unchanged by the named-import guard.
    let feature = "const express = require('express');\nconst router = express.Router();\nfunction list(req, res) { res.json({}); }\nrouter.get('/list', list);\nmodule.exports = { router };\n";
    let app = "import express from 'express';\nimport { router } from './feature';\nconst app = express();\napp.use('/api', router);\n";
    assert_eq!(
        route_names(&build_files(&[
            ("src/feature.js", feature),
            ("src/app.js", app)
        ])),
        ["GET /api/list", "USE /api"]
    );
}

#[test]
fn test_route_express_mount_without_router_argument_composes_nothing() {
    assert_eq!(routes_of("app.use('/api');"), ["GET /x", "USE /api"]);
}

#[test]
fn test_route_express_mount_past_the_prefix_cap_keeps_the_route() {
    // Stands for: the cap bounds the composed paths (16) and never drops the
    // endpoint: the route stays reachable at its own path as well.
    let tail: String = (0..20)
        .map(|i| format!("app.use('/m{i}', router);\n"))
        .collect();
    let names = routes_of(&tail);
    let gets: Vec<&String> = names.iter().filter(|n| n.starts_with("GET ")).collect();
    assert_eq!(
        gets.len(),
        17,
        "16 composed paths and the route's own: {names:?}"
    );
    assert!(gets.iter().any(|n| n.as_str() == "GET /x"), "{names:?}");
}
