//! TypeScript route identity: the producers that describe one (file, verb,
//! path) make one Route node (C1 `RouteKey`), and the handler edge starts
//! at the handler the registration declares (C4 `RawRoute.handler`), never
//! at the first node of that name.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::route_detector::{BS_KIND_ROUTE_HANDLER_ABSENT, BS_KIND_ROUTE_UNBOUND_HANDLER};
use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::identity::HandlerRef;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// Parse through the pipeline, which binds record endpoints, then build.
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

/// `(handler name, handler start line, printed handler kind)` of each
/// `HandlesRoute` edge into the one Route node named `route`.
fn handlers(g: &ZeroCopyGraph, route: &str) -> Vec<(String, u32, &'static str)> {
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
            )
        })
        .collect();
    out.sort();
    out
}

/// `(kind, hint)` of each route-handler BlindSpot.
fn handler_spots(g: &ZeroCopyGraph) -> Vec<(String, String)> {
    g.blind_spots
        .iter()
        .map(|bs| {
            (
                bs.kind.resolve(&g.string_pool).to_string(),
                bs.hint.resolve(&g.string_pool).to_string(),
            )
        })
        .filter(|(kind, _)| {
            kind == BS_KIND_ROUTE_UNBOUND_HANDLER || kind == BS_KIND_ROUTE_HANDLER_ABSENT
        })
        .collect()
}

const NESTJS: &str = r#"import { Controller, Get, Post } from '@nestjs/common';

@Controller('users')
export class UsersController {
  @Get('/list')
  list() {
    return [];
  }

  @Post('/create')
  create() {
    return 1;
  }

  @Get(':id')
  find() {
    return 2;
  }
}
"#;

/// The method decorator and the NestJS registration record both describe
/// `GET /list`; on HEAD each minted a Route node with the same uid.
#[test]
fn test_route_nestjs_one_node_per_route() {
    let g = build("src/users.controller.ts", NESTJS);
    assert_eq!(
        route_names(&g),
        vec!["GET /:id", "GET /list", "POST /create"],
        "one Route node per (file, verb, path)"
    );
    assert_eq!(handlers(&g, "GET /list"), vec![("list".into(), 6, "named")]);
    assert_eq!(
        handlers(&g, "POST /create"),
        vec![("create".into(), 11, "named")]
    );
}

/// A bare NestJS path (`@Get(':id')`) has no decorator route; the record's
/// handler is the method the decorator decorates. On HEAD it had none.
#[test]
fn test_route_nestjs_bare_path_binds_decorated_method() {
    let g = build("src/users.controller.ts", NESTJS);
    assert_eq!(handlers(&g, "GET /:id"), vec![("find".into(), 16, "named")]);
    assert_eq!(handler_spots(&g), Vec::<(String, String)>::new());
}

/// A `@Get` method of a class without `@Controller` is no NestJS route
/// (fixture `nestjs_controller.ts`, class `NotAController`). Its path
/// equals a controller route's path, so HEAD merged it into that Route
/// node and bound a second handler to it.
#[test]
fn test_route_nestjs_method_outside_controller_class_no_route() {
    let src = r#"import { Controller, Get } from '@nestjs/common';

@Controller('users')
export class UsersController {
  @Get(':id')
  find() {
    return 2;
  }
}

export class NotAController {
  @Get(':id')
  notARoute(id: string) {
    return id;
  }
}
"#;
    let g = build("src/users.controller.ts", src);
    assert_eq!(route_names(&g), vec!["GET /:id"]);
    assert_eq!(handlers(&g, "GET /:id"), vec![("find".into(), 6, "named")]);
}

/// `list` is both a class member (first in the file) and a top-level
/// function. The bare name `list` at the registration is the top-level
/// function; HEAD bound the first node named `list`, the member.
#[test]
fn test_route_two_same_name_handlers_binds_declared() {
    let src = r#"import express from 'express';
const app = express();

class Admin {
  list() {
    return 'admin';
  }
}

function list(req, res) {
  res.send('top');
}

app.get('/items', list);
"#;
    let g = build("src/app.ts", src);
    assert_eq!(route_names(&g), vec!["GET /items"]);
    assert_eq!(
        handlers(&g, "GET /items"),
        vec![("list".into(), 10, "named")]
    );
}

/// An arrow handler whose body calls something has a closure node: the
/// edge starts there and prints `inline_anonymous`. An arrow without a call
/// has no node and no BlindSpot, so `ecp routes` reads it as inline. A
/// name that binds nothing in the file is reported, never read as inline.
#[test]
fn test_route_express_inline_and_unbound_handlers_reported() {
    let src = r#"import express from 'express';
import { remote } from './remote';
const app = express();

function audit(req, res, next) {
  next();
}

app.get('/closure', audit, (req, res) => {
  res.send('x');
});
app.get('/empty', () => 1);
app.get('/remote', remote);
"#;
    let g = build("src/app.ts", src);
    let closure = handlers(&g, "GET /closure");
    assert_eq!(closure.len(), 1, "the last argument only: {closure:?}");
    assert!(closure[0].0.starts_with("<anonymous:9:"), "{closure:?}");
    assert_eq!(closure[0].2, "inline_anonymous");
    assert!(handlers(&g, "GET /empty").is_empty());
    assert!(handlers(&g, "GET /remote").is_empty());
    assert_eq!(
        handler_spots(&g),
        vec![(
            BS_KIND_ROUTE_UNBOUND_HANDLER.to_string(),
            "GET /remote names handler `remote`, which binds no declaration of this file"
                .to_string()
        )]
    );
}
