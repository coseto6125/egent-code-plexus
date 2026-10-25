//! Ruby route identity: one Route node per (file, verb, path) (C1 `RouteKey`),
//! and the handler edge starts at the block written at the registration (C4 `RawRoute.handler`).

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::route_detector::{BS_KIND_ROUTE_HANDLER_ABSENT, BS_KIND_ROUTE_UNBOUND_HANDLER};
use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ecp_core::contracts::identity::HandlerRef;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

/// Parse through the pipeline, which binds record endpoints, then build.
fn build(path: &str, src: &str) -> ZeroCopyGraph {
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(RubyProvider::new().unwrap()));
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

/// A Sinatra block whose body calls something has a closure node: the
/// edge starts there and prints `inline_anonymous`. HEAD gave the route no
/// edge.
#[test]
fn test_route_sinatra_block_handler_kind_inline_anonymous() {
    let src = r#"require 'sinatra'

get '/hello' do
  greet('x')
end

def greet(name)
  "hi #{name}"
end
"#;
    let g = build("app.rb", src);
    assert_eq!(route_names(&g), vec!["GET /hello"]);
    let hello = handlers(&g, "GET /hello");
    assert_eq!(hello.len(), 1, "{hello:?}");
    assert!(hello[0].0.starts_with("<anonymous:3:"), "{hello:?}");
    assert_eq!(
        (hello[0].2, hello[0].3.as_str()),
        ("inline_anonymous", "inline-closure")
    );
    assert_eq!(handler_spot_kinds(&g), Vec::<String>::new());
}

/// A Rails route names `controller#action` in another file: no node of
/// `config/routes.rb` binds it, so the route reports the name as
/// unresolved instead of reading as an inline handler. HEAD recorded
/// nothing.
#[test]
fn test_route_rails_controller_action_reports_unbound_handler() {
    let src = "Rails.application.routes.draw do\n  resources :posts, only: [:index]\nend\n";
    let g = build("config/routes.rb", src);
    assert_eq!(route_names(&g), vec!["GET /posts"]);
    assert!(handlers(&g, "GET /posts").is_empty());
    let hints: Vec<String> = g
        .blind_spots
        .iter()
        .filter(|bs| bs.kind.resolve(&g.string_pool) == BS_KIND_ROUTE_UNBOUND_HANDLER)
        .map(|bs| bs.hint.resolve(&g.string_pool).to_string())
        .collect();
    assert_eq!(
        hints,
        vec!["GET /posts names handler `posts#index`, which binds no declaration of this file"]
    );
}
