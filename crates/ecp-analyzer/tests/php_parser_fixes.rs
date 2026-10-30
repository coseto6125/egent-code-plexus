//! `PhpProvider` route emission after the dedupe moved from linear scans to
//! hash sets. The keys are `(method, call node id)` for verb calls and the
//! call node id for route sets. Output order is first sighting, verb calls
//! before route sets.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawRoute;
use std::path::Path;

/// `(method, path, handler)` of every route in `body`, in emitted order.
fn emitted(body: &str) -> Vec<(String, String, Option<String>)> {
    let src = format!("<?php\nuse Illuminate\\Support\\Facades\\Route;\n{body}\n");
    PhpProvider::new()
        .expect("PhpProvider::new")
        .parse_file(Path::new("routes/web.php"), src.as_bytes())
        .expect("parse_file")
        .routes
        .into_iter()
        .filter(|r| r.method != RawRoute::CONTROLLER_MIDDLEWARE)
        .map(|r| {
            let handler = r.handler_name().map(str::to_string);
            (r.method, r.path, handler)
        })
        .collect()
}

fn row(method: &str, path: &str, handler: Option<&str>) -> (String, String, Option<String>) {
    (
        method.to_string(),
        path.to_string(),
        handler.map(str::to_string),
    )
}

#[test]
fn test_php_route_calls_emit_once_each_in_source_order() {
    assert_eq!(
        emitted("Route::get('/a', 'A@x');\nRoute::post('/b', 'B@y');\nRoute::get('/c', 'C@z');"),
        [
            row("GET", "/a", Some("A@x")),
            row("POST", "/b", Some("B@y")),
            row("GET", "/c", Some("C@z")),
        ]
    );
}

/// The key is the call node, not `(method, path)`: two textually identical
/// calls are two routes, and a chained call is one route, not one per
/// matching pattern.
#[test]
fn test_php_duplicated_route_call_text_keeps_both_nodes_in_order() {
    assert_eq!(
        emitted("Route::get('/dup', 'A@x');\nRoute::get('/dup', 'B@y');"),
        [
            row("GET", "/dup", Some("A@x")),
            row("GET", "/dup", Some("B@y")),
        ]
    );
    assert_eq!(
        emitted("Route::middleware('auth')->get('/chained', 'C@m');"),
        [row("GET", "/chained", Some("C@m"))]
    );
}

/// The first string argument is the path; a later string argument never
/// replaces it, and a route call nested in another route's closure is its
/// own route after the outer one.
#[test]
fn test_php_route_call_first_string_argument_wins_as_path() {
    let first = emitted("Route::get('/first', '/second');");
    assert_eq!(first.len(), 1);
    assert_eq!(
        (first[0].0.as_str(), first[0].1.as_str()),
        ("GET", "/first")
    );
    let nested =
        emitted("Route::get('/outer', function () {\n    return Route::get('/inner', 'C@m');\n});");
    let paths: Vec<&str> = nested.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(paths, ["/outer", "/inner"]);
}

/// Route sets register after every verb call, whatever their source
/// position, and each set call registers its routes once.
#[test]
fn test_php_route_sets_follow_route_calls_and_register_once() {
    let routes = emitted(
        "Route::resource('photos', 'P')->only('index');\n\
         Route::get('/a', 'A@x');\n\
         Route::match(['put'], '/m', 'M@x');\n\
         Route::put('/z', 'Z@x');",
    );
    assert_eq!(routes.len(), 4, "{routes:?}");
    assert_eq!(routes[0], row("GET", "/a", Some("A@x")));
    assert_eq!(routes[1], row("PUT", "/z", Some("Z@x")));
    assert_eq!(routes[2].0, "GET");
    assert_eq!(routes[2].1, "/photos");
    assert_eq!(routes[3], row("PUT", "/m", Some("M@x")));
}

#[test]
fn test_php_match_route_set_emits_each_verb_once() {
    let mut routes = emitted("Route::match(['get', 'post'], '/form', 'F@h');");
    routes.sort();
    assert_eq!(
        routes,
        [
            row("GET", "/form", Some("F@h")),
            row("POST", "/form", Some("F@h")),
        ]
    );
}
