//! Laravel route groups: a route's path carries every enclosing group
//! prefix, including the prefix of a group in another file that loads the
//! route file; its middleware lists every group's middleware, outermost
//! first; and a controller action (`'C@m'`, `[C::class, 'm']`, `C::class`)
//! binds a `HandlesRoute` edge from the controller method the namespace in
//! scope names, or from the one class of that name when no namespace is in
//! scope, and from nothing when neither picks exactly one class. A class
//! that inherits the action binds the trait's or parent's method through
//! resolved heritage, and each bound method gets an `EntryPoint`.
//! `Route::resource` / `apiResource` / `match` / `any` register their
//! routes, and a route file without a framework import counts only when an
//! importing file loads it. `withoutMiddleware` / `excluded_middleware`
//! remove middleware by exact name, a controller's own middleware
//! (`$this->middleware` with `only` / `except`, static `HasMiddleware`)
//! joins the routes its actions handle, and a group's controller reaches an
//! included file's bare actions. Controller middleware that only the
//! runtime decides joins nothing and leaves a BlindSpot in the controller's
//! file.

use ecp_analyzer::php::parser::PhpProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{RawRoute, RouteInclude};
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PROVIDER: (&str, &str) = (
    "app/Providers/RouteServiceProvider.php",
    r#"<?php
namespace App\Providers;

use Illuminate\Foundation\Support\Providers\RouteServiceProvider as ServiceProvider;
use Illuminate\Support\Facades\Route;

class RouteServiceProvider extends ServiceProvider
{
    protected $namespace = 'App\Http\Controllers';

    public function map()
    {
        $this->mapAdminRoutes();
    }

    protected function mapAdminRoutes()
    {
        Route::prefix('admin')
            ->middleware('api')
            ->namespace($this->namespace)
            ->group(base_path('routes/admin.php'));
    }
}
"#,
);

const ADMIN_ROUTES: (&str, &str) = (
    "routes/admin.php",
    r#"<?php

use Illuminate\Support\Facades\Route;

Route::post('/login', 'AuthController@login');

Route::group(['middleware' => 'auth:admin'], function () {
    Route::post('/upload', 'FileController@upload');

    Route::prefix('import')
        ->group(function () {
            Route::post('/award', 'AwardController@import');
        });
});
"#,
);

const FILE_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/FileController.php",
    r#"<?php
namespace App\Http\Controllers;

class FileController extends Controller
{
    public function upload() {}
}
"#,
);

const AWARD_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/AwardController.php",
    r#"<?php
namespace App\Http\Controllers;

class AwardController extends Controller
{
    public function import() {}
}
"#,
);

const AUTH_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/AuthController.php",
    r#"<?php
namespace App\Http\Controllers;

class AuthController extends Controller
{
    public function login() {}
}
"#,
);

const USER_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/UserController.php",
    r#"<?php
namespace App\Http\Controllers;

class UserController extends Controller
{
    public function index() {}
}
"#,
);

const ADMIN_USER_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/Admin/UserController.php",
    r#"<?php
namespace App\Http\Controllers\Admin;

class UserController extends Controller
{
    public function index() {}
}
"#,
);

/// The route and include records of a file. Its controller-middleware
/// records feed the builder, and the graph tests below check them.
fn parse(path: &str, src: &str) -> Vec<RawRoute> {
    PhpProvider::new()
        .unwrap()
        .parse_file(Path::new(path), src.as_bytes())
        .unwrap()
        .routes
        .into_iter()
        .filter(|route| route.method != RawRoute::CONTROLLER_MIDDLEWARE)
        .collect()
}

fn graph(files: &[(&str, &str)]) -> ZeroCopyGraph {
    let provider = PhpProvider::new().unwrap();
    let mut builder = GraphBuilder::new();
    for (path, src) in files {
        builder.add_graph(
            provider
                .parse_file(Path::new(path), src.as_bytes())
                .unwrap(),
        );
    }
    builder.build()
}

fn name(graph: &ZeroCopyGraph, idx: usize) -> &str {
    graph.nodes[idx].name.resolve(&graph.string_pool)
}

/// Every Route node's name, sorted.
fn route_names(graph: &ZeroCopyGraph) -> Vec<&str> {
    let mut names: Vec<&str> = (0..graph.nodes.len())
        .filter(|&idx| graph.nodes[idx].kind == NodeKind::Route)
        .map(|idx| name(graph, idx))
        .collect();
    names.sort_unstable();
    names
}

fn route_idx(graph: &ZeroCopyGraph, route: &str) -> usize {
    (0..graph.nodes.len())
        .find(|&idx| graph.nodes[idx].kind == NodeKind::Route && name(graph, idx) == route)
        .unwrap_or_else(|| panic!("no Route `{route}` in {:?}", route_names(graph)))
}

/// `(file, owner, name, reason)` of each `HandlesRoute` source of `route`.
fn handlers(graph: &ZeroCopyGraph, route: &str) -> Vec<(String, String, String, String)> {
    let target = route_idx(graph, route) as u32;
    graph
        .edges
        .iter()
        .filter(|edge| edge.rel_type == RelType::HandlesRoute && edge.target == target)
        .map(|edge| {
            let node = &graph.nodes[edge.source as usize];
            (
                graph.files[node.file_idx as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_string(),
                node.owner_class.resolve(&graph.string_pool).to_string(),
                node.name.resolve(&graph.string_pool).to_string(),
                edge.reason.resolve(&graph.string_pool).to_string(),
            )
        })
        .collect()
}

fn handler(
    file: &str,
    owner: &str,
    method: &str,
    reason: &str,
) -> (String, String, String, String) {
    (file.into(), owner.into(), method.into(), reason.into())
}

fn middleware(graph: &ZeroCopyGraph, route: &str) -> Vec<String> {
    let idx = route_idx(graph, route) as u32;
    graph
        .route_middleware
        .iter()
        .find(|entry| entry.node_idx == idx)
        .map(|entry| {
            entry
                .middleware
                .iter()
                .map(|m| m.resolve(&graph.string_pool).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn routes_file(body: &str) -> String {
    format!("<?php\nuse Illuminate\\Support\\Facades\\Route;\n{body}\n")
}

// ── Parser: the in-file view ────────────────────────────────────────────

#[test]
fn test_parse_file_provider_group_base_path_emits_include_record() {
    let routes = parse(PROVIDER.0, PROVIDER.1);
    assert_eq!(routes.len(), 1, "{routes:?}");
    let record = &routes[0];
    assert_eq!(record.method, RawRoute::INCLUDE);
    assert_eq!(record.path, "/admin");
    let scope = record.scope.as_deref().expect("include scope");
    assert_eq!(scope.middleware, ["api"]);
    assert_eq!(scope.namespace.as_deref(), Some("App\\Http\\Controllers"));
    assert_eq!(
        scope.include,
        Some(RouteInclude::AppRoot("routes/admin.php".into()))
    );
}

#[test]
fn test_parse_file_option_array_group_with_file_emits_include_record() {
    let src = routes_file(
        "Route::group(['prefix' => 'v1', 'middleware' => ['api', 'throttle:60,1'], 'namespace' => 'Api'], base_path('routes/v1.php'));",
    );
    let routes = parse("routes/api.php", &src);
    assert_eq!(routes.len(), 1, "{routes:?}");
    let scope = routes[0].scope.as_deref().unwrap();
    assert_eq!(routes[0].path, "/v1");
    assert_eq!(scope.middleware, ["api", "throttle:60,1"]);
    assert_eq!(scope.namespace.as_deref(), Some("Api"));
    assert_eq!(
        scope.include,
        Some(RouteInclude::AppRoot("routes/v1.php".into()))
    );
}

#[test]
fn test_parse_file_closure_group_emits_no_include_record() {
    let src =
        routes_file("Route::prefix('admin')->group(function () { Route::get('/x', 'C@m'); });");
    let routes = parse("routes/web.php", &src);
    let methods: Vec<&str> = routes.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(methods, ["GET"], "{routes:?}");
}

#[test]
fn test_parse_file_require_dir_relative_emits_file_dir_include() {
    let src =
        routes_file("require __DIR__.'/auth.php';\nrequire dirname(__DIR__).'/extra/more.php';");
    let routes = parse("routes/web.php", &src);
    let mut includes: Vec<String> = routes
        .iter()
        .filter_map(|r| match r.scope.as_deref()?.include.as_ref()? {
            RouteInclude::FileDir(path) => Some(path.clone()),
            RouteInclude::AppRoot(_) => None,
        })
        .collect();
    includes.sort();
    assert_eq!(includes, ["../extra/more.php", "auth.php"], "{routes:?}");
}

/// A route file that calls the global `Route` alias keeps its records,
/// marked: the builder keeps them only when an importing file loads it.
#[test]
fn test_parse_file_without_framework_import_marks_records_needs_include() {
    let src = "<?php\nRoute::prefix('admin')->group(base_path('routes/admin.php'));\nRoute::get('/x', 'C@m');\n";
    let routes = parse("routes/web.php", src);
    assert_eq!(routes.len(), 2, "{routes:?}");
    assert!(
        routes
            .iter()
            .all(|r| r.scope.as_deref().is_some_and(|scope| scope.needs_include)),
        "{routes:?}"
    );
}

#[test]
fn test_parse_file_with_framework_import_does_not_mark_needs_include() {
    let routes = parse("routes/web.php", &routes_file("Route::get('/x', 'C@m');"));
    assert!(!routes[0].scope.as_deref().unwrap().needs_include);
}

/// `Route` bound to the file's own class is that class, not the router.
#[test]
fn test_parse_file_own_route_class_without_framework_import_emits_nothing() {
    let imported = "<?php\nuse App\\Support\\Route;\nRoute::get('/x', 'C@m');\n";
    let aliased = "<?php\nuse App\\Support\\Cache as Route;\nRoute::get('/x', 'C@m');\n";
    let declared =
        "<?php\nclass Route { public static function get($k) {} }\nRoute::get('/x', 'C@m');\n";
    for src in [imported, aliased, declared] {
        assert!(parse("routes/web.php", src).is_empty(), "{src}");
    }
}

#[test]
fn test_parse_file_nested_prefixes_join_with_single_slashes() {
    let src = routes_file(
        "Route::prefix('/api/')->group(function () {
            Route::group(['prefix' => 'v1/'], function () {
                Route::get('users/', 'UserController@index');
                Route::get('', 'UserController@index');
            });
        });",
    );
    let mut paths: Vec<String> = parse("routes/api.php", &src)
        .into_iter()
        .map(|r| r.path)
        .collect();
    paths.sort();
    assert_eq!(paths, ["/api/v1", "/api/v1/users"]);
}

#[test]
fn test_parse_file_empty_prefix_leaves_path_unchanged() {
    let src = routes_file("Route::prefix('')->group(function () { Route::get('/ping', 'C@m'); });");
    let paths: Vec<String> = parse("routes/web.php", &src)
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(paths, ["/ping"]);
}

#[test]
fn test_parse_file_middleware_string_array_and_trailing_in_outer_to_inner_order() {
    let src = routes_file(
        "Route::middleware('web')->group(function () {
            Route::group(['middleware' => ['auth', 'verified']], function () {
                Route::get('/me', 'C@m')->middleware('throttle:60,1');
            });
        });",
    );
    let routes = parse("routes/web.php", &src);
    assert_eq!(routes.len(), 1, "{routes:?}");
    assert_eq!(routes[0].path, "/me");
    assert_eq!(
        routes[0].scope.as_deref().unwrap().middleware,
        ["web", "auth", "verified", "throttle:60,1"]
    );
}

#[test]
fn test_parse_file_middleware_group_without_prefix_keeps_path() {
    let src =
        routes_file("Route::middleware('auth')->group(function () { Route::get('/me', 'C@m'); });");
    let routes = parse("routes/web.php", &src);
    assert_eq!(routes[0].path, "/me");
    assert_eq!(routes[0].scope.as_deref().unwrap().middleware, ["auth"]);
}

#[test]
fn test_parse_file_route_outside_groups_has_no_middleware() {
    let routes = parse(
        "routes/web.php",
        routes_file("Route::get('/open', 'C@m');").as_str(),
    );
    assert!(routes[0].scope.as_deref().unwrap().middleware.is_empty());
}

#[test]
fn test_parse_file_action_forms_name_class_and_method() {
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\UserController;

Route::get('/a', 'UserController@index');
Route::get('/b', [UserController::class, 'show']);
Route::get('/c', ShowDashboard::class);
Route::get('/d', '\App\Http\Controllers\UserController@edit');
Route::get('/e', function () { return 1; });
Route::controller(OrderController::class)->group(function () {
    Route::get('/f', 'list');
});
"#;
    let mut handlers: Vec<(String, Option<String>)> = parse("routes/web.php", src)
        .into_iter()
        .map(|r| (r.path, r.handler))
        .collect();
    handlers.sort();
    let expected = [
        ("/a", Some("UserController@index")),
        ("/b", Some("\\App\\Http\\Controllers\\UserController@show")),
        // A class constant names an invokable class, never a method of a
        // group controller.
        ("/c", Some("ShowDashboard@__invoke")),
        ("/d", Some("\\App\\Http\\Controllers\\UserController@edit")),
        ("/e", None),
        ("/f", Some("OrderController@list")),
    ]
    .map(|(path, handler)| (path.to_string(), handler.map(str::to_string)));
    assert_eq!(handlers, expected);
}

/// `(method, path, handler)` of each route `body` registers, sorted.
fn sites(body: &str) -> Vec<(String, String, Option<String>)> {
    let mut sites: Vec<_> = parse("routes/web.php", &routes_file(body))
        .into_iter()
        .map(|r| (r.method, r.path, r.handler))
        .collect();
    sites.sort();
    sites
}

/// `(method, path)` of each route `body` registers, sorted.
fn site_paths(body: &str) -> Vec<(String, String)> {
    sites(body)
        .into_iter()
        .map(|(method, path, _)| (method, path))
        .collect()
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = expected
        .iter()
        .map(|(method, path)| (method.to_string(), path.to_string()))
        .collect();
    pairs.sort();
    pairs
}

#[test]
fn test_parse_file_resource_emits_every_action_with_conventional_paths() {
    let mut expected = vec![
        ("GET", "/photos", "index"),
        ("GET", "/photos/create", "create"),
        ("POST", "/photos", "store"),
        ("GET", "/photos/{photo}", "show"),
        ("GET", "/photos/{photo}/edit", "edit"),
        ("PUT", "/photos/{photo}", "update"),
        ("PATCH", "/photos/{photo}", "update"),
        ("DELETE", "/photos/{photo}", "destroy"),
    ]
    .into_iter()
    .map(|(method, path, action)| {
        (
            method.to_string(),
            path.to_string(),
            Some(format!("PhotoController@{action}")),
        )
    })
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        sites("Route::resource('photos', 'PhotoController');"),
        expected
    );
}

#[test]
fn test_parse_file_api_resource_omits_create_and_edit() {
    assert_eq!(
        site_paths("Route::apiResource('photos', PhotoController::class);"),
        pairs(&[
            ("GET", "/photos"),
            ("POST", "/photos"),
            ("GET", "/photos/{photo}"),
            ("PUT", "/photos/{photo}"),
            ("PATCH", "/photos/{photo}"),
            ("DELETE", "/photos/{photo}"),
        ])
    );
}

#[test]
fn test_parse_file_resource_class_constant_resolves_through_import() {
    let src = "use App\\Http\\Controllers\\PhotoController;\nRoute::resource('photos', PhotoController::class)->only('index');";
    assert_eq!(
        sites(src),
        [(
            "GET".to_string(),
            "/photos".to_string(),
            Some("\\App\\Http\\Controllers\\PhotoController@index".to_string())
        )]
    );
}

#[test]
fn test_parse_file_resource_only_keeps_listed_actions() {
    assert_eq!(
        site_paths("Route::resource('photos', 'C')->only(['index', 'show']);"),
        pairs(&[("GET", "/photos"), ("GET", "/photos/{photo}")])
    );
    assert_eq!(
        site_paths("Route::resource('photos', 'C')->only('index', 'store');"),
        pairs(&[("GET", "/photos"), ("POST", "/photos")])
    );
}

#[test]
fn test_parse_file_resource_except_drops_listed_actions() {
    assert_eq!(
        site_paths("Route::apiResource('photos', 'C')->except(['update', 'destroy']);"),
        pairs(&[
            ("GET", "/photos"),
            ("POST", "/photos"),
            ("GET", "/photos/{photo}")
        ])
    );
}

/// Laravel applies `only`, then `except`; both in the options array work
/// the same as both chained.
#[test]
fn test_parse_file_resource_only_and_except_together_apply_only_then_except() {
    let expected = pairs(&[("GET", "/photos"), ("DELETE", "/photos/{photo}")]);
    assert_eq!(
        site_paths(
            "Route::resource('photos', 'C')->only(['index', 'show', 'destroy'])->except('show');"
        ),
        expected
    );
    assert_eq!(
        site_paths("Route::resource('photos', 'C', ['only' => ['index', 'show', 'destroy'], 'except' => ['show']]);"),
        expected
    );
}

/// A later `only` replaces an earlier one, as the pending registration's
/// option array does.
#[test]
fn test_parse_file_resource_second_only_replaces_first() {
    assert_eq!(
        site_paths("Route::resource('photos', 'C')->only(['index'])->only(['store']);"),
        pairs(&[("POST", "/photos")])
    );
}

#[test]
fn test_parse_file_nested_resource_nests_under_parent_wildcard() {
    let paths = site_paths("Route::resource('photos.comments', 'PhotoCommentController');");
    assert_eq!(paths.len(), 8, "{paths:?}");
    for expected in [
        ("GET", "/photos/{photo}/comments"),
        ("GET", "/photos/{photo}/comments/create"),
        ("GET", "/photos/{photo}/comments/{comment}"),
        ("GET", "/photos/{photo}/comments/{comment}/edit"),
    ] {
        assert!(
            paths.contains(&(expected.0.into(), expected.1.into())),
            "{paths:?}"
        );
    }
}

#[test]
fn test_parse_file_shallow_nested_resource_drops_parent_from_member_routes() {
    assert_eq!(
        site_paths("Route::resource('photos.comments', 'C')->only(['index', 'show'])->shallow();"),
        pairs(&[
            ("GET", "/photos/{photo}/comments"),
            ("GET", "/comments/{comment}")
        ])
    );
}

#[test]
fn test_parse_file_resource_parameters_rename_wildcard() {
    assert_eq!(
        site_paths(
            "Route::resource('photos', 'C')->only('show')->parameters(['photos' => 'pic']);"
        ),
        pairs(&[("GET", "/photos/{pic}")])
    );
}

/// Wildcards are Laravel's `Str::singular` of the segment: `-` becomes
/// `_`, an uncountable word stays, and a leading `/` or a `prefix/`
/// segment of the name is a prefix, not part of the wildcard.
#[test]
fn test_parse_file_resource_wildcard_singularizes_segment() {
    let cases = [
        (
            "product_categories",
            "/product_categories/{product_category}",
        ),
        ("/bus", "/bus/{bus}"),
        ("buses", "/buses/{bus}"),
        ("news", "/news/{news}"),
        ("boxes", "/boxes/{box}"),
        ("addresses", "/addresses/{address}"),
        ("people", "/people/{person}"),
        ("support-tickets", "/support-tickets/{support_ticket}"),
        ("admin/photos", "/admin/photos/{photo}"),
    ];
    for (resource, path) in cases {
        let body = format!("Route::resource('{resource}', 'C')->only('show');");
        assert_eq!(site_paths(&body), pairs(&[("GET", path)]), "{resource}");
    }
}

#[test]
fn test_parse_file_resource_in_group_carries_prefix_namespace_and_middleware() {
    let src = routes_file(
        "Route::prefix('admin')->middleware('auth')->namespace('Admin')->group(function () {
            Route::resource('photos', 'PhotoController')->only('index')->middleware('can:list');
        });",
    );
    let routes = parse("routes/web.php", &src);
    assert_eq!(routes.len(), 1, "{routes:?}");
    assert_eq!(routes[0].path, "/admin/photos");
    let scope = routes[0].scope.as_deref().unwrap();
    assert_eq!(scope.middleware, ["auth", "can:list"]);
    assert_eq!(scope.namespace.as_deref(), Some("Admin"));
}

#[test]
fn test_parse_file_resources_registers_each_resource() {
    assert_eq!(
        site_paths("Route::resources(['photos' => 'PhotoController', 'posts' => 'PostController'], ['only' => ['index']]);"),
        pairs(&[("GET", "/photos"), ("GET", "/posts")])
    );
    assert_eq!(
        site_paths("Route::apiResources(['photos' => 'PhotoController']);").len(),
        6
    );
}

#[test]
fn test_parse_file_resource_on_non_router_emits_nothing() {
    assert!(
        sites("Cache::resource('photos', 'C');\n$registrar->resource('photos', 'C');").is_empty()
    );
}

#[test]
fn test_parse_file_resource_with_non_literal_name_emits_nothing() {
    assert!(sites("Route::resource($name, 'C');").is_empty());
}

#[test]
fn test_parse_file_match_emits_one_route_per_verb() {
    let handler = Some("FormController@handle".to_string());
    assert_eq!(
        sites("Route::match(['get', 'POST', 'get'], '/form', 'FormController@handle');"),
        [
            ("GET".to_string(), "/form".to_string(), handler.clone()),
            ("POST".to_string(), "/form".to_string(), handler),
        ]
    );
    assert_eq!(
        site_paths("Route::match('put', '/one', 'C@m');"),
        pairs(&[("PUT", "/one")])
    );
}

#[test]
fn test_parse_file_match_with_empty_verb_list_emits_nothing() {
    assert!(sites("Route::match([], '/form', 'C@m');").is_empty());
}

#[test]
fn test_parse_file_any_emits_one_any_route() {
    assert_eq!(
        sites("Route::prefix('hooks')->group(function () { Route::any('/stripe', 'HookController@stripe'); });"),
        [(
            "ANY".to_string(),
            "/hooks/stripe".to_string(),
            Some("HookController@stripe".to_string())
        )]
    );
}

// ── Builder: across files ───────────────────────────────────────────────

#[test]
fn test_build_provider_include_prefixes_and_scopes_included_routes() {
    let graph = graph(&[
        PROVIDER,
        ADMIN_ROUTES,
        FILE_CONTROLLER,
        AWARD_CONTROLLER,
        AUTH_CONTROLLER,
    ]);
    assert_eq!(
        route_names(&graph),
        [
            "POST /admin/import/award",
            "POST /admin/login",
            "POST /admin/upload"
        ]
    );
    assert_eq!(
        middleware(&graph, "POST /admin/import/award"),
        ["api", "auth:admin"]
    );
    assert_eq!(middleware(&graph, "POST /admin/login"), ["api"]);
}

#[test]
fn test_build_string_action_with_provider_namespace_binds_controller_method() {
    let graph = graph(&[
        PROVIDER,
        ADMIN_ROUTES,
        FILE_CONTROLLER,
        AWARD_CONTROLLER,
        AUTH_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "POST /admin/upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action"
        )]
    );
    assert_eq!(
        handlers(&graph, "POST /admin/import/award"),
        [handler(
            AWARD_CONTROLLER.0,
            "AwardController",
            "import",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_action_naming_unknown_class_binds_nothing() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/x', 'MissingController@show');").as_str(),
        ),
        FILE_CONTROLLER,
    ]);
    assert!(handlers(&graph, "GET /x").is_empty());
}

#[test]
fn test_build_action_naming_missing_method_binds_nothing() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/x', 'FileController@missing');").as_str(),
        ),
        FILE_CONTROLLER,
    ]);
    assert!(handlers(&graph, "GET /x").is_empty());
}

#[test]
fn test_build_duplicate_class_names_namespace_picks_the_named_one() {
    let src = routes_file(
        "Route::namespace('App\\Http\\Controllers\\Admin')->group(function () {
            Route::get('/users', 'UserController@index');
        });",
    );
    let graph = graph(&[
        ("routes/web.php", src.as_str()),
        USER_CONTROLLER,
        ADMIN_USER_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "GET /users"),
        [handler(
            ADMIN_USER_CONTROLLER.0,
            "UserController",
            "index",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_duplicate_class_names_without_namespace_binds_nothing() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/users', 'UserController@index');").as_str(),
        ),
        USER_CONTROLLER,
        ADMIN_USER_CONTROLLER,
    ]);
    assert!(handlers(&graph, "GET /users").is_empty());
}

#[test]
fn test_build_unique_class_name_without_namespace_binds_by_name() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/upload', 'FileController@upload');").as_str(),
        ),
        FILE_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "GET /upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action:unique-name"
        )]
    );
}

#[test]
fn test_build_array_action_through_import_picks_the_imported_class() {
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\Admin\UserController;

Route::get('/users', [UserController::class, 'index']);
"#;
    let graph = graph(&[
        ("routes/web.php", src),
        USER_CONTROLLER,
        ADMIN_USER_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "GET /users"),
        [handler(
            ADMIN_USER_CONTROLLER.0,
            "UserController",
            "index",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_invokable_class_action_binds_invoke() {
    let controller = (
        "app/Http/Controllers/ShowDashboard.php",
        "<?php\nnamespace App\\Http\\Controllers;\nclass ShowDashboard { public function __invoke() {} }\n",
    );
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\ShowDashboard;

Route::get('/dashboard', ShowDashboard::class);
"#;
    let graph = graph(&[("routes/web.php", src), controller]);
    assert_eq!(
        handlers(&graph, "GET /dashboard"),
        [handler(
            controller.0,
            "ShowDashboard",
            "__invoke",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_controller_group_binds_bare_method_name() {
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\FileController;

Route::controller(FileController::class)->group(function () {
    Route::post('/upload', 'upload');
});
"#;
    let graph = graph(&[("routes/web.php", src), FILE_CONTROLLER]);
    assert_eq!(
        handlers(&graph, "POST /upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_closure_action_binds_nothing() {
    let graph = graph(&[(
        "routes/web.php",
        routes_file("Route::get('/x', function () { return 1; });").as_str(),
    )]);
    assert!(handlers(&graph, "GET /x").is_empty());
}

#[test]
fn test_build_include_target_missing_leaves_other_routes_unchanged() {
    let provider = routes_file("Route::prefix('admin')->group(base_path('routes/missing.php'));");
    let web = routes_file("Route::get('/x', 'C@m');");
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        ("routes/web.php", web.as_str()),
    ]);
    assert_eq!(route_names(&graph), ["GET /x"]);
    assert!(middleware(&graph, "GET /x").is_empty());
}

/// A route file two groups load is reachable under both prefixes, so it
/// gets a Route node under each.
#[test]
fn test_build_route_file_included_by_two_groups_emits_route_under_each() {
    let provider = routes_file(
        "Route::prefix('v1')->middleware('api')->group(base_path('routes/api.php'));
        Route::prefix('v2')->group(base_path('routes/api.php'));",
    );
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        (
            "routes/api.php",
            routes_file("Route::get('/items', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /v1/items", "GET /v2/items"]);
    assert_eq!(middleware(&graph, "GET /v1/items"), ["api"]);
    assert!(middleware(&graph, "GET /v2/items").is_empty());
}

/// Two groups with the same prefix give one path; one Route node stands
/// for both. Laravel's `RouteCollection::addToCollections` keys routes by
/// method and URI, so the route registered last replaces the first: the
/// node carries the last group's middleware.
#[test]
fn test_build_two_includes_with_same_prefix_emit_one_route() {
    let provider = routes_file(
        "Route::prefix('v1')->middleware('api')->group(base_path('routes/api.php'));
        Route::prefix('v1')->middleware('web')->group(base_path('routes/api.php'));",
    );
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        (
            "routes/api.php",
            routes_file("Route::get('/items', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /v1/items"]);
    assert_eq!(middleware(&graph, "GET /v1/items"), ["web"]);
}

#[test]
fn test_build_require_chain_carries_scope_through_two_files() {
    let provider = routes_file("Route::middleware('web')->group(__DIR__.'/../../routes/web.php');");
    let web = routes_file(
        "Route::get('/', 'C@m');\nRoute::prefix('account')->group(function () { require __DIR__.'/auth.php'; });",
    );
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        ("routes/web.php", web.as_str()),
        (
            "routes/auth.php",
            routes_file("Route::get('/login', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /", "GET /account/login"]);
    assert_eq!(middleware(&graph, "GET /account/login"), ["web"]);
}

#[test]
fn test_build_laravel_11_with_routing_prefixes_api_routes() {
    let app = r#"<?php
use Illuminate\Foundation\Application;

return Application::configure(basePath: dirname(__DIR__))
    ->withRouting(
        web: __DIR__.'/../routes/web.php',
        api: __DIR__.'/../routes/api.php',
    )
    ->create();
"#;
    let graph = graph(&[
        ("bootstrap/app.php", app),
        (
            "routes/web.php",
            routes_file("Route::get('/home', 'C@m');").as_str(),
        ),
        (
            "routes/api.php",
            routes_file("Route::get('/user', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /api/user", "GET /home"]);
    assert_eq!(middleware(&graph, "GET /api/user"), ["api"]);
    assert_eq!(middleware(&graph, "GET /home"), ["web"]);
}

/// `base_path()` is the nearest application root at or above the provider:
/// a repo with two Laravel apps resolves each provider to its own routes.
#[test]
fn test_build_base_path_resolves_to_the_providers_own_app() {
    let provider = routes_file("Route::prefix('api')->group(base_path('routes/api.php'));");
    let graph = graph(&[
        (
            "shop/app/Providers/RouteServiceProvider.php",
            provider.as_str(),
        ),
        (
            "shop/routes/api.php",
            routes_file("Route::get('/cart', 'C@m');").as_str(),
        ),
        (
            "blog/routes/api.php",
            routes_file("Route::get('/posts', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /api/cart", "GET /posts"]);
}

const PHOTO_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/PhotoController.php",
    r#"<?php
namespace App\Http\Controllers;

class PhotoController extends Controller
{
    public function index() {}
    public function show() {}
    public function update() {}
}
"#,
);

/// A base controller the other controllers of the namespace extend.
const BASE_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/Controller.php",
    r#"<?php
namespace App\Http\Controllers;

class Controller
{
    public function upload() {}
    public function destroy() {}
}
"#,
);

/// `(file, owner, method, reason)` of each `EntryPoint` marker's target.
fn entry_points(graph: &ZeroCopyGraph) -> Vec<(String, String, String, String)> {
    let mut out: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.rel_type == RelType::References
                && graph.nodes[edge.source as usize].kind == NodeKind::EntryPoint
        })
        .map(|edge| {
            let marker = &graph.nodes[edge.source as usize];
            let target = &graph.nodes[edge.target as usize];
            (
                graph.files[marker.file_idx as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_string(),
                target.owner_class.resolve(&graph.string_pool).to_string(),
                target.name.resolve(&graph.string_pool).to_string(),
                edge.reason.resolve(&graph.string_pool).to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn test_build_resource_binds_only_the_actions_the_controller_declares() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::resource('photos', 'PhotoController');").as_str(),
        ),
        PHOTO_CONTROLLER,
    ]);
    assert_eq!(route_names(&graph).len(), 8, "{:?}", route_names(&graph));
    assert_eq!(
        handlers(&graph, "GET /photos/{photo}"),
        [handler(
            PHOTO_CONTROLLER.0,
            "PhotoController",
            "show",
            "controller-action:unique-name"
        )]
    );
    for route in ["PUT /photos/{photo}", "PATCH /photos/{photo}"] {
        assert_eq!(handlers(&graph, route).len(), 1, "{route}");
    }
    for route in [
        "POST /photos",
        "GET /photos/create",
        "DELETE /photos/{photo}",
    ] {
        assert!(handlers(&graph, route).is_empty(), "{route}");
    }
}

#[test]
fn test_build_resource_on_unknown_controller_emits_routes_without_edges() {
    let graph = graph(&[(
        "routes/web.php",
        routes_file("Route::apiResource('photos', 'MissingController');").as_str(),
    )]);
    let routes = route_names(&graph);
    assert_eq!(routes.len(), 6, "{routes:?}");
    for route in routes {
        assert!(handlers(&graph, route).is_empty(), "{route}");
    }
}

#[test]
fn test_build_resource_in_included_file_carries_include_prefix_and_namespace() {
    let routes = r#"<?php
use Illuminate\Support\Facades\Route;

Route::resource('photos', 'PhotoController')->only(['index']);
"#;
    let graph = graph(&[PROVIDER, ("routes/admin.php", routes), PHOTO_CONTROLLER]);
    assert_eq!(route_names(&graph), ["GET /admin/photos"]);
    assert_eq!(middleware(&graph, "GET /admin/photos"), ["api"]);
    assert_eq!(
        handlers(&graph, "GET /admin/photos"),
        [handler(
            PHOTO_CONTROLLER.0,
            "PhotoController",
            "index",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_any_route_binds_handler() {
    let controller = (
        "app/Http/Controllers/HookController.php",
        "<?php\nnamespace App\\Http\\Controllers;\nclass HookController { public function stripe() {} }\n",
    );
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::any('/stripe', 'HookController@stripe');").as_str(),
        ),
        controller,
    ]);
    assert_eq!(route_names(&graph), ["ANY /stripe"]);
    assert_eq!(handlers(&graph, "ANY /stripe").len(), 1);
}

/// The client layout: the provider imports Illuminate, the route file it
/// loads calls the global `Route` alias without a `use` line.
#[test]
fn test_build_included_route_file_without_import_emits_scoped_routes() {
    let admin = "<?php\nRoute::post('/upload', 'FileController@upload');\n";
    let graph = graph(&[PROVIDER, ("routes/admin.php", admin), FILE_CONTROLLER]);
    assert_eq!(route_names(&graph), ["POST /admin/upload"]);
    assert_eq!(middleware(&graph, "POST /admin/upload"), ["api"]);
    assert_eq!(
        handlers(&graph, "POST /admin/upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action"
        )]
    );
}

/// Admission passes through a chain of files without an import.
#[test]
fn test_build_require_chain_without_imports_from_admitted_provider_emits_routes() {
    let admin = "<?php\nRoute::prefix('users')->group(function () { require __DIR__.'/admin/users.php'; });\n";
    let users = "<?php\nRoute::get('/list', 'C@m');\n";
    let graph = graph(&[
        PROVIDER,
        ("routes/admin.php", admin),
        ("routes/admin/users.php", users),
    ]);
    assert_eq!(route_names(&graph), ["GET /admin/users/list"]);
}

#[test]
fn test_build_route_file_without_import_nothing_loads_emits_nothing() {
    let graph = graph(&[(
        "routes/admin.php",
        "<?php\nRoute::post('/upload', 'FileController@upload');\n",
    )]);
    assert!(route_names(&graph).is_empty());
}

/// A `require` in plain PHP code admits nothing: the file that requires is
/// not a framework file either.
#[test]
fn test_build_route_file_required_only_by_files_without_import_emits_nothing() {
    let graph = graph(&[
        ("index.php", "<?php\nrequire __DIR__.'/lib/routes.php';\n"),
        ("lib/routes.php", "<?php\nRoute::get('/x', 'C@m');\n"),
    ]);
    assert!(route_names(&graph).is_empty());
}

#[test]
fn test_build_action_inherited_from_parent_class_binds_parent_method() {
    let routes = r#"<?php
use Illuminate\Support\Facades\Route;

Route::delete('/files/{id}', 'FileController@destroy');
"#;
    let graph = graph(&[
        PROVIDER,
        ("routes/admin.php", routes),
        FILE_CONTROLLER,
        BASE_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "DELETE /admin/files/{id}"),
        [handler(
            BASE_CONTROLLER.0,
            "Controller",
            "destroy",
            "controller-action:inherited"
        )]
    );
}

/// The class's own method wins over the parent's of the same name.
#[test]
fn test_build_action_declared_by_class_and_parent_binds_own_method() {
    let graph = graph(&[PROVIDER, ADMIN_ROUTES, FILE_CONTROLLER, BASE_CONTROLLER]);
    assert_eq!(
        handlers(&graph, "POST /admin/upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action"
        )]
    );
}

#[test]
fn test_build_action_from_used_trait_binds_trait_method() {
    let trait_file = (
        "app/Http/Controllers/Exports.php",
        "<?php\nnamespace App\\Http\\Controllers;\ntrait Exports { public function export() {} }\n",
    );
    let controller = (
        "app/Http/Controllers/ReportController.php",
        "<?php\nnamespace App\\Http\\Controllers;\nclass ReportController { use Exports; }\n",
    );
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/export', 'ReportController@export');").as_str(),
        ),
        trait_file,
        controller,
    ]);
    assert_eq!(
        handlers(&graph, "GET /export"),
        [handler(
            trait_file.0,
            "Exports",
            "export",
            "controller-action:unique-name:inherited"
        )]
    );
}

/// The parent is outside the graph (`Illuminate\Routing\Controller`), so
/// nothing says which class declares the method.
#[test]
fn test_build_action_missing_with_unresolved_parent_binds_nothing() {
    let controller = (
        "app/Http/Controllers/Controller.php",
        "<?php\nnamespace App\\Http\\Controllers;\nuse Illuminate\\Routing\\Controller as BaseController;\nclass Controller extends BaseController {}\n",
    );
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/x', 'Controller@middleware');").as_str(),
        ),
        controller,
    ]);
    assert!(handlers(&graph, "GET /x").is_empty());
}

#[test]
fn test_build_cross_file_action_marks_controller_method_entry_point() {
    let graph = graph(&[PROVIDER, ADMIN_ROUTES, FILE_CONTROLLER, AWARD_CONTROLLER]);
    assert_eq!(
        entry_points(&graph),
        [
            (
                AWARD_CONTROLLER.0.to_string(),
                "AwardController".to_string(),
                "import".to_string(),
                "route:1.00:route:POST /admin/import/award".to_string()
            ),
            (
                FILE_CONTROLLER.0.to_string(),
                "FileController".to_string(),
                "upload".to_string(),
                "route:1.00:route:POST /admin/upload".to_string()
            ),
        ]
    );
}

/// `update` handles PUT and PATCH: one marker carries both routes.
#[test]
fn test_build_method_handling_two_routes_gets_one_entry_point() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::resource('photos', 'PhotoController')->only('update');").as_str(),
        ),
        PHOTO_CONTROLLER,
    ]);
    assert_eq!(
        entry_points(&graph),
        [(
            PHOTO_CONTROLLER.0.to_string(),
            "PhotoController".to_string(),
            "update".to_string(),
            "route:0.90:route:PUT /photos/{photo}; also: route:PATCH /photos/{photo}".to_string()
        )]
    );
}

#[test]
fn test_build_unbound_action_marks_no_entry_point() {
    let graph = graph(&[
        (
            "routes/web.php",
            routes_file("Route::get('/x', 'FileController@missing');").as_str(),
        ),
        FILE_CONTROLLER,
    ]);
    assert!(entry_points(&graph).is_empty());
}

// ── Review fixes: include depth, removed middleware, controller middleware ─

/// `dirname(__DIR__, n)` outside PHP's 1.. range, or past 64 levels in all,
/// names no route file; an unchecked count overflowed the path built from
/// it (`capacity overflow`) or allocated gigabytes. Any PHP file reaches it.
#[test]
fn test_parse_file_dirname_levels_outside_1_to_64_emits_no_include() {
    let src = r#"<?php
require dirname(__DIR__, 18446744073709551615) . "/x.php";
require dirname(__DIR__, 1000000000) . "/x.php";
require dirname(__DIR__, 0) . "/x.php";
require dirname(dirname(__DIR__, 64), 1) . "/x.php";
require dirname(__DIR__, 2) . "/ok.php";
"#;
    let includes: Vec<RouteInclude> = parse("app/boom.php", src)
        .into_iter()
        .filter_map(|r| r.scope?.include)
        .collect();
    assert_eq!(includes, [RouteInclude::FileDir("../../ok.php".into())]);
}

#[test]
fn test_parse_file_without_middleware_calls_record_excluded_middleware() {
    let src = routes_file(
        "Route::withoutMiddleware('a')->withoutMiddleware(['b'])->group(function () {
            Route::get('/x', 'C@m')->withoutMiddleware('c');
        });
        Route::group(['excluded_middleware' => ['d'], 'withoutMiddleware' => 'e'], function () {
            Route::get('/y', 'C@m');
        });",
    );
    let excluded: Vec<(String, Vec<String>)> = parse("routes/web.php", &src)
        .into_iter()
        .map(|r| (r.path, r.scope.unwrap().excluded_middleware))
        .collect();
    // `Router::group` reads only `excluded_middleware` from an option
    // array; `withoutMiddleware` is a `RouteRegistrar` method name.
    assert_eq!(
        excluded,
        [
            ("/x".to_string(), vec!["a".into(), "b".into(), "c".into()]),
            ("/y".to_string(), vec!["d".into()]),
        ]
    );
}

#[test]
fn test_parse_file_variadic_middleware_call_records_every_argument() {
    let src = routes_file(
        "Route::middleware('a', 'b')->group(function () {
            Route::get('/x', 'C@m')->middleware('c', 'd');
        });",
    );
    let routes = parse("routes/web.php", &src);
    assert_eq!(
        routes[0].scope.as_deref().unwrap().middleware,
        ["a", "b", "c", "d"]
    );
}

const HOOK_PROVIDER: (&str, &str) = (
    "app/Providers/RouteServiceProvider.php",
    r#"<?php
namespace App\Providers;
use Illuminate\Support\Facades\Route;
class RouteServiceProvider {
    protected $namespace = 'App\Http\Controllers';
    public function map() {
        Route::prefix('api')->middleware('api')->namespace($this->namespace)->group(base_path('routes/api.php'));
        Route::middleware('web')->namespace($this->namespace)->group(base_path('routes/legacy.php'));
    }
}
"#,
);

const HOOK_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/HookController.php",
    r#"<?php
namespace App\Http\Controllers;
class HookController {
    public function handle() {}
    public function me() {}
}
"#,
);

/// A route's `->withoutMiddleware('auth')` removes the group's `auth`:
/// Laravel runs `/api/hook` with `api` only.
#[test]
fn test_build_route_without_middleware_drops_group_middleware() {
    let api = routes_file(
        "Route::middleware('auth')->group(function () {
            Route::post('/hook', 'HookController@handle')->withoutMiddleware('auth');
            Route::get('/me', 'HookController@me');
        });",
    );
    let graph = graph(&[
        HOOK_PROVIDER,
        ("routes/api.php", api.as_str()),
        HOOK_CONTROLLER,
    ]);
    assert_eq!(middleware(&graph, "POST /api/hook"), ["api"]);
    assert_eq!(middleware(&graph, "GET /api/me"), ["api", "auth"]);
}

/// A group in the included file removes the including group's middleware;
/// a `withoutMiddleware` option key removes nothing.
#[test]
fn test_build_group_excluded_middleware_drops_include_middleware() {
    let api = routes_file(
        "Route::withoutMiddleware('api')->group(function () {
            Route::get('/open', 'HookController@me');
        });
        Route::group(['excluded_middleware' => ['api']], function () {
            Route::get('/also-open', 'HookController@me');
        });
        Route::group(['withoutMiddleware' => 'api'], function () {
            Route::get('/closed', 'HookController@me');
        });",
    );
    let graph = graph(&[
        HOOK_PROVIDER,
        ("routes/api.php", api.as_str()),
        HOOK_CONTROLLER,
    ]);
    assert!(middleware(&graph, "GET /api/open").is_empty());
    assert!(middleware(&graph, "GET /api/also-open").is_empty());
    assert_eq!(middleware(&graph, "GET /api/closed"), ["api"]);
}

/// Laravel matches a removed name exactly once both resolve to classes:
/// `auth:admin` resolves to `Authenticate:admin`, `auth` to
/// `Authenticate`, so `withoutMiddleware('auth')` keeps `auth:admin`.
#[test]
fn test_build_without_middleware_bare_name_keeps_parameterized_middleware() {
    let src = routes_file(
        "Route::middleware(['auth:admin', 'auth'])->group(function () {
            Route::get('/x', 'C@m')->withoutMiddleware('auth');
        });",
    );
    let graph = graph(&[("routes/web.php", src.as_str())]);
    assert_eq!(middleware(&graph, "GET /x"), ["auth:admin"]);
}

#[test]
fn test_build_middleware_named_twice_listed_once() {
    let src = routes_file(
        "Route::middleware('auth')->group(function () {
            Route::get('/x', 'C@m')->middleware('auth');
        });",
    );
    let graph = graph(&[("routes/web.php", src.as_str())]);
    assert_eq!(middleware(&graph, "GET /x"), ["auth"]);
}

/// A route file that imports the global `Route` facade alias (`use Route;`)
/// calls the router itself, so a loading provider keeps its routes.
#[test]
fn test_build_global_route_alias_import_keeps_loaded_routes() {
    let legacy = "<?php\nuse Route;\nRoute::get('/legacy', 'HookController@me');\n";
    let graph = graph(&[
        HOOK_PROVIDER,
        ("routes/legacy.php", legacy),
        HOOK_CONTROLLER,
    ]);
    assert_eq!(route_names(&graph), ["GET /legacy"]);
    assert_eq!(middleware(&graph, "GET /legacy"), ["web"]);
}

/// `use App\Support\Route;` binds `Route` to the app's own class.
#[test]
fn test_parse_file_namespaced_route_import_emits_no_route() {
    let src = "<?php\nuse App\\Support\\Route;\nRoute::get('/x', 'C@m');\n";
    assert!(parse("routes/web.php", src).is_empty());
}

/// `use Foo as Route;` binds `Route` to another class: no router call.
#[test]
fn test_parse_file_global_class_renamed_route_emits_no_route() {
    let src = "<?php\nuse Foo as Route;\nRoute::get('/x', 'C@m');\n";
    assert!(parse("routes/web.php", src).is_empty());
}

/// `Route::controller(C::class)->group(<file>)`: a bare action of the
/// included file names a method of `C`.
#[test]
fn test_build_group_controller_reaches_included_file_bare_action() {
    let provider = r#"<?php
namespace App\Providers;
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\HookController;
class RouteServiceProvider {
    public function map() {
        Route::controller(HookController::class)->prefix('hooks')->group(base_path('routes/hooks.php'));
    }
}
"#;
    let hooks = routes_file("Route::post('/run', 'handle');");
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider),
        ("routes/hooks.php", hooks.as_str()),
        HOOK_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "POST /hooks/run"),
        [handler(
            HOOK_CONTROLLER.0,
            "HookController",
            "handle",
            "controller-action"
        )]
    );
}

/// Laravel's `prependGroupNamespace` leaves an action that already starts
/// with the group namespace as written.
#[test]
fn test_build_qualified_action_under_group_namespace_binds_without_second_prefix() {
    let src = routes_file(
        "Route::namespace('App\\Http\\Controllers')->group(function () {
            Route::get('/u', 'App\\Http\\Controllers\\UserController@index');
        });",
    );
    let graph = graph(&[
        ("routes/web.php", src.as_str()),
        USER_CONTROLLER,
        ADMIN_USER_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "GET /u"),
        [handler(
            USER_CONTROLLER.0,
            "UserController",
            "index",
            "controller-action"
        )]
    );
}

/// With no namespace in scope, a class name equal to one class's fully
/// qualified name pins that class at full confidence.
#[test]
fn test_build_qualified_action_without_namespace_binds_exact_class() {
    let src = routes_file("Route::get('/u', 'App\\Http\\Controllers\\UserController@index');");
    let graph = graph(&[
        ("routes/web.php", src.as_str()),
        USER_CONTROLLER,
        ADMIN_USER_CONTROLLER,
    ]);
    assert_eq!(
        handlers(&graph, "GET /u"),
        [handler(
            USER_CONTROLLER.0,
            "UserController",
            "index",
            "controller-action"
        )]
    );
    let target = route_idx(&graph, "GET /u") as u32;
    let edge = graph
        .edges
        .iter()
        .find(|e| e.rel_type == RelType::HandlesRoute && e.target == target)
        .unwrap();
    assert_eq!(edge.confidence, 1.0);
}

/// More include scopes than the cap reach a file: the routes under the
/// dropped scopes have no Route node, and a BlindSpot says so.
#[test]
fn test_build_include_scopes_past_cap_record_blind_spot() {
    let provider = routes_file(
        &(1..=17)
            .map(|n| format!("Route::prefix('p{n}')->group(base_path('routes/api.php'));"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        (
            "routes/api.php",
            routes_file("Route::get('/x', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph).len(), 16);
    let spots: Vec<(&str, &str)> = graph
        .blind_spots
        .iter()
        .map(|bs| {
            (
                bs.kind.resolve(&graph.string_pool),
                bs.file_path.resolve(&graph.string_pool),
            )
        })
        .filter(|(kind, _)| *kind == "laravel-include-scope-cap")
        .collect();
    assert_eq!(spots, [("laravel-include-scope-cap", "routes/api.php")]);
    let hint = graph
        .blind_spots
        .iter()
        .find(|bs| bs.kind.resolve(&graph.string_pool) == "laravel-include-scope-cap")
        .map(|bs| bs.hint.resolve(&graph.string_pool))
        .unwrap();
    assert!(hint.starts_with("1 more include scopes"), "{hint}");
}

#[test]
fn test_build_include_scopes_within_cap_record_no_blind_spot() {
    let graph = graph(&[PROVIDER, ADMIN_ROUTES]);
    assert!(graph
        .blind_spots
        .iter()
        .all(|bs| bs.kind.resolve(&graph.string_pool) != "laravel-include-scope-cap"));
}

const ADMIN_PROVIDER: (&str, &str) = (
    "app/Providers/RouteServiceProvider.php",
    r#"<?php
namespace App\Providers;
use Illuminate\Support\Facades\Route;
class RouteServiceProvider {
    protected $namespace = 'App\Http\Controllers';
    public function map() {
        Route::prefix('admin')->middleware('api')->namespace($this->namespace)->group(base_path('routes/admin.php'));
    }
}
"#,
);

/// The client case: `AuthController`'s constructor guards every action but
/// the `except` list with `auth:admin`.
#[test]
fn test_build_constructor_middleware_except_skips_listed_actions() {
    let controller = r#"<?php
namespace App\Http\Controllers;
class AuthController extends Controller
{
    public function __construct()
    {
        $this->middleware('auth:admin', ['except' => ['login', 'login2', 'logintest']]);
    }
    public function login2() {}
    public function getSidebarMenus() {}
}
"#;
    let routes = routes_file(
        "Route::post('/login', 'AuthController@login2');
        Route::get('/user-menus', 'AuthController@getSidebarMenus');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/AuthController.php", controller),
    ]);
    assert_eq!(middleware(&graph, "POST /admin/login"), ["api"]);
    assert_eq!(
        middleware(&graph, "GET /admin/user-menus"),
        ["api", "auth:admin"]
    );
}

#[test]
fn test_build_constructor_middleware_chained_only_except_select_actions() {
    let controller = r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware('log')->only('index');
        $this->middleware(['throttle', 'verified'])->except(['index']);
    }
    public function index() {}
    public function show() {}
}
"#;
    let routes = routes_file(
        "Route::get('/posts', 'PostController@index');
        Route::get('/posts/{post}', 'PostController@show');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/posts"), ["api", "log"]);
    assert_eq!(
        middleware(&graph, "GET /admin/posts/{post}"),
        ["api", "throttle", "verified"]
    );
}

/// A `HasMiddleware` controller's static `middleware()` decides; Laravel
/// then reads no constructor middleware.
#[test]
fn test_build_has_middleware_static_entries_follow_only_and_except() {
    let controller = r#"<?php
namespace App\Http\Controllers;
use Illuminate\Routing\Controllers\HasMiddleware;
use Illuminate\Routing\Controllers\Middleware;
class PostController implements HasMiddleware
{
    public function __construct()
    {
        $this->middleware('ignored');
    }
    public static function middleware(): array
    {
        return [
            'auth',
            new Middleware('log', only: ['index']),
            (new Middleware('subscribed'))->except('index'),
            new Middleware(['a', 'b'], null, ['index']),
        ];
    }
    public function index() {}
    public function show() {}
}
"#;
    let routes = routes_file(
        "Route::get('/posts', 'PostController@index');
        Route::get('/posts/{post}', 'PostController@show');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    assert_eq!(
        middleware(&graph, "GET /admin/posts"),
        ["api", "auth", "log"]
    );
    assert_eq!(
        middleware(&graph, "GET /admin/posts/{post}"),
        ["api", "auth", "subscribed", "a", "b"]
    );
}

const BASE_ADMIN_CONTROLLER: (&str, &str) = (
    "app/Http/Controllers/AdminController.php",
    r#"<?php
namespace App\Http\Controllers;
class AdminController
{
    public function __construct()
    {
        $this->middleware('auth:admin');
    }
}
"#,
);

/// PHP runs the nearest declared constructor; a parent's runs only when the
/// child has none or calls `parent::__construct()`.
#[test]
fn test_build_parent_constructor_middleware_follows_php_constructor_rules() {
    let inherits = r#"<?php
namespace App\Http\Controllers;
class ReportController extends AdminController
{
    public function index() {}
}
"#;
    let calls_parent = r#"<?php
namespace App\Http\Controllers;
class ExportController extends AdminController
{
    public function __construct()
    {
        parent::__construct();
        $this->middleware('log');
    }
    public function index() {}
}
"#;
    let skips_parent = r#"<?php
namespace App\Http\Controllers;
class OpenController extends AdminController
{
    public function __construct()
    {
        $this->middleware('log');
    }
    public function index() {}
}
"#;
    let routes = routes_file(
        "Route::get('/reports', 'ReportController@index');
        Route::get('/exports', 'ExportController@index');
        Route::get('/open', 'OpenController@index');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        BASE_ADMIN_CONTROLLER,
        ("app/Http/Controllers/ReportController.php", inherits),
        ("app/Http/Controllers/ExportController.php", calls_parent),
        ("app/Http/Controllers/OpenController.php", skips_parent),
    ]);
    assert_eq!(
        middleware(&graph, "GET /admin/reports"),
        ["api", "auth:admin"]
    );
    assert_eq!(
        middleware(&graph, "GET /admin/exports"),
        ["api", "auth:admin", "log"]
    );
    assert_eq!(middleware(&graph, "GET /admin/open"), ["api", "log"]);
}

/// Laravel removes excluded middleware after it adds the controller's, and
/// lists a name the route already has once.
#[test]
fn test_build_controller_middleware_respects_route_exclusion_and_duplicates() {
    let controller = r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware(['api', 'auth', 'log']);
    }
    public function index() {}
}
"#;
    let routes =
        routes_file("Route::get('/posts', 'PostController@index')->withoutMiddleware('auth');");
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/posts"), ["api", "log"]);
}

/// A controller middleware closure has no name and adds nothing; a route
/// whose action binds nothing gets no controller middleware.
#[test]
fn test_build_controller_middleware_closure_or_unbound_action_adds_nothing() {
    let controller = r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware(function ($request, $next) { return $next($request); });
    }
    public function index() {}
}
"#;
    let routes = routes_file(
        "Route::get('/posts', 'PostController@index');
        Route::get('/missing', 'PostController@missing');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/posts"), ["api"]);
    assert_eq!(middleware(&graph, "GET /admin/missing"), ["api"]);
}

/// A controller-middleware record is no endpoint and no entry point.
#[test]
fn test_build_controller_middleware_record_emits_no_route_or_entry_point() {
    let controller = r#"<?php
namespace App\Http\Controllers;
use Illuminate\Support\Facades\Route;
class PostController
{
    public function __construct()
    {
        $this->middleware('auth');
    }
    public function index() {}
}
"#;
    let graph = graph(&[("app/Http/Controllers/PostController.php", controller)]);
    assert!(route_names(&graph).is_empty());
    assert!(entry_points(&graph).is_empty());
}

/// A client `fetch` of any verb `Route::any` registers reaches the `ANY`
/// route; a verb it does not register does not.
#[test]
fn test_build_fetch_reaches_any_route_for_laravel_verb_only() {
    let tmp = tempfile::tempdir().unwrap();
    let client = "async function load() {\n  await fetch('/ping', { method: 'POST' });\n  await fetch('/pong', { method: 'TRACE' });\n}\n";
    std::fs::write(tmp.path().join("client.ts"), &client).unwrap();
    let routes = routes_file("Route::any('/ping', 'C@m');\nRoute::any('/pong', 'C@m');");
    let php = PhpProvider::new().unwrap();
    let ts = ecp_analyzer::typescript::parser::TypeScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(
        php.parse_file(Path::new("routes/web.php"), routes.as_bytes())
            .unwrap(),
    );
    builder.add_graph(
        ts.parse_file(Path::new("client.ts"), client.as_bytes())
            .unwrap(),
    );
    let graph = builder.build();
    let fetched: Vec<&str> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Fetches)
        .map(|e| name(&graph, e.target as usize))
        .collect();
    assert_eq!(fetched, ["ANY /ping"]);
}

const DYNAMIC: &str = "laravel-controller-middleware-dynamic";

/// `(file, hint)` of each BlindSpot of `kind`, sorted.
fn blind_spots<'g>(graph: &'g ZeroCopyGraph, kind: &str) -> Vec<(&'g str, &'g str)> {
    let mut spots: Vec<(&str, &str)> = graph
        .blind_spots
        .iter()
        .filter(|bs| bs.kind.resolve(&graph.string_pool) == kind)
        .map(|bs| {
            (
                bs.file_path.resolve(&graph.string_pool),
                bs.hint.resolve(&graph.string_pool),
            )
        })
        .collect();
    spots.sort_unstable();
    spots
}

/// The middleware of `GET /admin/posts`, bound to `PostController@index`
/// of `controller`, and the controller-middleware BlindSpots.
fn post_controller_middleware(controller: &str) -> (Vec<String>, Vec<(String, String)>) {
    let routes = routes_file("Route::get('/posts', 'PostController@index');");
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    let spots = blind_spots(&graph, DYNAMIC)
        .into_iter()
        .map(|(file, hint)| (file.to_string(), hint.to_string()))
        .collect();
    (middleware(&graph, "GET /admin/posts"), spots)
}

/// `use Router;` imports a global class `Router`. Laravel registers no
/// global `Router` alias, so `Router::get` is that class's method.
#[test]
fn test_parse_file_global_router_import_emits_no_route() {
    let src = "<?php\nuse Router;\nRouter::get('/x', 'C@m');\n";
    assert!(parse("routes/web.php", src).is_empty());
}

/// Laravel's `controllerMiddleware` calls the static `middleware()` only of
/// a `HasMiddleware` class; without the interface the constructor decides.
#[test]
fn test_build_static_middleware_without_has_middleware_adds_nothing() {
    let (middleware, spots) = post_controller_middleware(
        r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public static function middleware(): array
    {
        return ['auth'];
    }
    public function index() {}
}
"#,
    );
    assert_eq!(middleware, ["api"]);
    assert!(spots.is_empty(), "{spots:?}");
}

/// The interface on a parent class of the graph makes the child a
/// `HasMiddleware`, and the parent's static `middleware()` decides.
#[test]
fn test_build_has_middleware_on_parent_reads_inherited_static_middleware() {
    let base = r#"<?php
namespace App\Http\Controllers;
use Illuminate\Routing\Controllers\HasMiddleware;
class GuardedController implements HasMiddleware
{
    public static function middleware(): array
    {
        return ['auth'];
    }
}
"#;
    let child = "<?php\nnamespace App\\Http\\Controllers;\nclass PostController extends GuardedController { public function index() {} }\n";
    let routes = routes_file("Route::get('/posts', 'PostController@index');");
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/GuardedController.php", base),
        ("app/Http/Controllers/PostController.php", child),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/posts"), ["api", "auth"]);
    assert!(blind_spots(&graph, DYNAMIC).is_empty());
}

/// `Controller::middleware` appends each registration, so they run in
/// source order.
#[test]
fn test_build_constructor_middleware_keeps_source_order() {
    let (middleware, _) = post_controller_middleware(
        r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware('first');
        $this->middleware('second');
    }
    public function index() {}
}
"#,
    );
    assert_eq!(middleware, ["api", "first", "second"]);
}

/// The parent's registrations run where `parent::__construct()` stands: a
/// child registration before the call comes first.
#[test]
fn test_build_parent_constructor_middleware_runs_at_parent_call() {
    let late = r#"<?php
namespace App\Http\Controllers;
class LateController extends AdminController
{
    public function __construct()
    {
        $this->middleware('child');
        parent::__construct();
        $this->middleware('after');
    }
    public function index() {}
}
"#;
    let routes = routes_file("Route::get('/late', 'LateController@index');");
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        BASE_ADMIN_CONTROLLER,
        ("app/Http/Controllers/LateController.php", late),
    ]);
    assert_eq!(
        middleware(&graph, "GET /admin/late"),
        ["api", "child", "auth:admin", "after"]
    );
    assert!(blind_spots(&graph, DYNAMIC).is_empty());
}

/// A closure, an arrow function, a nested function or an anonymous class
/// in the constructor runs nothing when the constructor does.
#[test]
fn test_build_constructor_middleware_in_nested_scope_adds_nothing() {
    let (middleware, spots) = post_controller_middleware(
        r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $later = function () { $this->middleware('closure'); };
        $arrow = fn () => $this->middleware('arrow');
        function helper() { $this->middleware('nested'); }
        $other = new class { public function __construct() { $this->middleware('anonymous'); } };
        $this->middleware('auth');
    }
    public function index() {}
}
"#,
    );
    assert_eq!(middleware, ["api", "auth"]);
    assert!(spots.is_empty(), "{spots:?}");
}

/// A registration under a branch may not run: the controller adds no
/// middleware, and a BlindSpot in its file says why.
#[test]
fn test_build_conditional_constructor_middleware_records_blind_spot() {
    let (middleware, spots) = post_controller_middleware(
        r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware('auth');
        if (config('app.strict')) {
            $this->middleware('strict');
        }
    }
    public function index() {}
}
"#,
    );
    assert_eq!(middleware, ["api"]);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert_eq!(spots[0].0, "app/Http/Controllers/PostController.php");
    assert!(spots[0].1.contains("branch"), "{spots:?}");
}

/// Laravel registers nothing after a constructor that returned, threw or
/// exited: a registration after a statement that may do so adds no
/// middleware, and a BlindSpot says why.
#[test]
fn test_build_constructor_middleware_after_possible_exit_records_blind_spot() {
    for exit in [
        "if ($this->off) return;",
        "if ($this->off) { throw new \\RuntimeException(); }",
        "$this->off && die('off');",
        "while (true) { exit; }",
        "return;",
    ] {
        let controller = format!(
            r#"<?php
namespace App\Http\Controllers;
class PostController
{{
    public function __construct()
    {{
        {exit}
        $this->middleware('auth');
    }}
    public function index() {{}}
}}
"#
        );
        let (middleware, spots) = post_controller_middleware(&controller);
        assert_eq!(middleware, ["api"], "{exit}");
        assert_eq!(spots.len(), 1, "{exit}: {spots:?}");
        assert!(spots[0].1.contains("may end the constructor"), "{spots:?}");
    }
}

/// `$exit` and `static::exit` name no exit, so the
/// registration after them counts.
#[test]
fn test_build_constructor_middleware_after_exit_named_variable_counts() {
    for statement in ["$exit = false;", "$die = static::exit;"] {
        let controller = format!(
            r#"<?php
namespace App\Http\Controllers;
class PostController
{{
    public function __construct()
    {{
        {statement}
        $this->middleware('auth');
    }}
    public function index() {{}}
}}
"#
        );
        let (middleware, spots) = post_controller_middleware(&controller);
        assert_eq!(middleware, ["api", "auth"], "{statement}");
        assert!(spots.is_empty(), "{statement}: {spots:?}");
    }
}

/// A registration before the early exit runs whenever the constructor
/// does; an exit inside a closure leaves only the closure.
#[test]
fn test_build_constructor_middleware_before_exit_or_after_closure_return_counts() {
    let (middleware, spots) = post_controller_middleware(
        r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware('auth');
        $check = function () { return; };
        $this->middleware('verified');
        if ($this->off) return;
    }
    public function index() {}
}
"#,
    );
    assert_eq!(middleware, ["api", "auth", "verified"]);
    assert!(spots.is_empty(), "{spots:?}");
}

/// A static `middleware()` with a return under a branch, or a second
/// return, is not one array literal: no middleware, a BlindSpot.
#[test]
fn test_build_static_middleware_not_one_array_records_blind_spot() {
    for body in [
        "if (app()->isLocal()) { return []; }\n        return ['auth'];",
        "return ['auth'];\n        return ['admin'];",
    ] {
        let controller = format!(
            r#"<?php
namespace App\Http\Controllers;
use Illuminate\Routing\Controllers\HasMiddleware;
class PostController implements HasMiddleware
{{
    public static function middleware(): array
    {{
        {body}
    }}
    public function index() {{}}
}}
"#
        );
        let (middleware, spots) = post_controller_middleware(&controller);
        assert_eq!(middleware, ["api"], "{body}");
        assert_eq!(spots.len(), 1, "{spots:?}");
        assert!(spots[0].1.contains("one array literal"), "{spots:?}");
    }
}

const GUARDS_TRAIT: (&str, &str) = (
    "app/Http/Controllers/GuardsAdmins.php",
    r#"<?php
namespace App\Http\Controllers;
trait GuardsAdmins
{
    public function __construct()
    {
        $this->middleware('trait');
    }
}
"#,
);

/// PHP runs a used trait's constructor before the parent's: the trait's
/// registrations count, the parent's do not. A trait outside the graph may
/// declare one, so its user adds nothing and gets a BlindSpot.
#[test]
fn test_build_trait_constructor_replaces_parent_constructor() {
    let with_trait = r#"<?php
namespace App\Http\Controllers;
class TraitController extends AdminController
{
    use GuardsAdmins;
    public function index() {}
}
"#;
    let with_vendor_trait = r#"<?php
namespace App\Http\Controllers;
use Vendor\Package\SetsUp;
class VendorTraitController extends AdminController
{
    use SetsUp;
    public function index() {}
}
"#;
    let routes = routes_file(
        "Route::get('/trait', 'TraitController@index');
        Route::get('/vendor', 'VendorTraitController@index');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        BASE_ADMIN_CONTROLLER,
        GUARDS_TRAIT,
        ("app/Http/Controllers/TraitController.php", with_trait),
        (
            "app/Http/Controllers/VendorTraitController.php",
            with_vendor_trait,
        ),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/trait"), ["api", "trait"]);
    assert_eq!(middleware(&graph, "GET /admin/vendor"), ["api"]);
    let spots = blind_spots(&graph, DYNAMIC);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert_eq!(spots[0].0, "app/Http/Controllers/VendorTraitController.php");
    assert!(spots[0].1.contains("Vendor\\Package\\SetsUp"), "{spots:?}");
}

/// An `only` / `except` list only the runtime knows may cover any method:
/// no middleware, a BlindSpot. The same holds for a static entry.
#[test]
fn test_build_non_literal_only_except_records_blind_spot() {
    let constructor = r#"<?php
namespace App\Http\Controllers;
class PostController
{
    public function __construct()
    {
        $this->middleware('auth')->except($this->open);
    }
    public function index() {}
}
"#;
    let (middleware, spots) = post_controller_middleware(constructor);
    assert_eq!(middleware, ["api"]);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert!(spots[0].1.contains("only` / `except"), "{spots:?}");

    let entry = r#"<?php
namespace App\Http\Controllers;
use Illuminate\Routing\Controllers\HasMiddleware;
use Illuminate\Routing\Controllers\Middleware;
class PostController implements HasMiddleware
{
    public static function middleware(): array
    {
        return ['log', new Middleware('auth', except: self::$skip)];
    }
    public function index() {}
}
"#;
    let (middleware, spots) = post_controller_middleware(entry);
    assert_eq!(middleware, ["api"]);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert!(spots[0].1.contains("entry"), "{spots:?}");
}

/// `X::class` names the class its own file resolves `X` to. A route-file
/// `Guard` and a controller `Guard` that resolve to different classes do
/// not cancel; the same class does.
#[test]
fn test_build_class_middleware_resolves_in_each_file_before_exclusion() {
    let routes = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Middleware\Guard;
Route::get('/posts', 'PostController@index')->withoutMiddleware(Guard::class);
Route::get('/open', 'PostController@index')->withoutMiddleware(\Vendor\Auth\Guard::class);
Route::get('/mine', 'PostController@index')->middleware(Guard::class);
"#;
    let controller = r#"<?php
namespace App\Http\Controllers;
use Vendor\Auth\Guard;
class PostController
{
    public function __construct()
    {
        $this->middleware(Guard::class);
    }
    public function index() {}
}
"#;
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes),
        ("app/Http/Controllers/PostController.php", controller),
    ]);
    assert_eq!(
        middleware(&graph, "GET /admin/posts"),
        ["api", "Vendor\\Auth\\Guard"]
    );
    assert_eq!(middleware(&graph, "GET /admin/open"), ["api"]);
    assert_eq!(
        middleware(&graph, "GET /admin/mine"),
        ["api", "App\\Http\\Middleware\\Guard", "Vendor\\Auth\\Guard"]
    );
}

/// In a `Route::controller(C::class)` group a class-constant action names
/// an invokable class, not a method of `C`.
#[test]
fn test_build_class_constant_action_in_controller_group_binds_invoke() {
    let page = (
        "app/Http/Controllers/Page.php",
        "<?php\nnamespace App\\Http\\Controllers;\nclass Page { public function __invoke() {} }\n",
    );
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\FileController;
use App\Http\Controllers\Page;

Route::controller(FileController::class)->group(function () {
    Route::get('/upload', 'upload');
    Route::get('/page', Page::class);
});
"#;
    let graph = graph(&[("routes/web.php", src), FILE_CONTROLLER, page]);
    assert_eq!(
        handlers(&graph, "GET /page"),
        [handler(page.0, "Page", "__invoke", "controller-action")]
    );
    assert_eq!(
        handlers(&graph, "GET /upload"),
        [handler(
            FILE_CONTROLLER.0,
            "FileController",
            "upload",
            "controller-action"
        )]
    );
}

/// Including one file under `api`, `web`, then `api` again registers its
/// routes under `api` last, so `api` wins.
#[test]
fn test_build_same_include_scope_repeated_last_registration_wins() {
    let provider = routes_file(
        "Route::prefix('v1')->middleware('api')->group(base_path('routes/api.php'));
        Route::prefix('v1')->middleware('web')->group(base_path('routes/api.php'));
        Route::prefix('v1')->middleware('api')->group(base_path('routes/api.php'));",
    );
    let graph = graph(&[
        ("app/Providers/RouteServiceProvider.php", provider.as_str()),
        (
            "routes/api.php",
            routes_file("Route::get('/items', 'C@m');").as_str(),
        ),
    ]);
    assert_eq!(route_names(&graph), ["GET /v1/items"]);
    assert_eq!(middleware(&graph, "GET /v1/items"), ["api"]);
}

/// The Route nodes a client's `fetch('/ping', { method: 'GET' })` reaches.
fn fetched_ping(routes_body: &str) -> Vec<String> {
    fetched(routes_body, "/ping")
}

/// The Route nodes a client's `fetch(url, { method: 'GET' })` reaches.
fn fetched(routes_body: &str, url: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let client =
        format!("async function load() {{\n  await fetch('{url}', {{ method: 'GET' }});\n}}\n");
    std::fs::write(tmp.path().join("client.ts"), &client).unwrap();
    let routes = routes_file(routes_body);
    let php = PhpProvider::new().unwrap();
    let ts = ecp_analyzer::typescript::parser::TypeScriptProvider::new().unwrap();
    let mut builder = GraphBuilder::new().with_repo_root(tmp.path().to_path_buf());
    builder.add_graph(
        php.parse_file(Path::new("routes/web.php"), routes.as_bytes())
            .unwrap(),
    );
    builder.add_graph(
        ts.parse_file(Path::new("client.ts"), client.as_bytes())
            .unwrap(),
    );
    let graph = builder.build();
    graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Fetches)
        .map(|e| name(&graph, e.target as usize).to_string())
        .collect()
}

/// `Route::any` files its route under every verb; a later `Route::get` of
/// the same path replaces it for GET.
#[test]
fn test_build_fetch_get_registered_after_any_reaches_get_route_only() {
    assert_eq!(
        fetched_ping("Route::any('/ping', 'Old@m');\nRoute::get('/ping', 'New@m');"),
        ["GET /ping"]
    );
}

/// A later `Route::any` replaces an earlier `Route::get` for GET.
#[test]
fn test_build_fetch_any_registered_after_get_reaches_any_route_only() {
    assert_eq!(
        fetched_ping("Route::get('/ping', 'Old@m');\nRoute::any('/ping', 'New@m');"),
        ["ANY /ping"]
    );
}

/// Laravel keys its routes by URI: `/items/{id}` and `/items/{slug}` are two
/// URIs, so a later `Route::get` of the second leaves the `ANY` route of the
/// first answering GET too.
#[test]
fn test_build_fetch_any_and_get_of_different_uris_reaches_both_routes() {
    let mut reached = fetched(
        "Route::any('/items/{id}', 'Old@m');\nRoute::get('/items/{slug}', 'New@m');",
        "/items/:id",
    );
    reached.sort_unstable();
    assert_eq!(reached, ["ANY /items/{id}", "GET /items/{slug}"]);
}

/// A parent class outside the graph may run its own constructor; Laravel's
/// `Illuminate\Routing\Controller` registers nothing.
#[test]
fn test_build_parent_outside_graph_records_blind_spot_unless_framework() {
    let vendor = r#"<?php
namespace App\Http\Controllers;
class VendorController extends \Vendor\Admin\CrudController
{
    public function index() {}
}
"#;
    let framework = r#"<?php
namespace App\Http\Controllers;
use Illuminate\Routing\Controller as BaseController;
class PlainController extends BaseController
{
    public function index() {}
}
"#;
    let routes = routes_file(
        "Route::get('/vendor', 'VendorController@index');
        Route::get('/plain', 'PlainController@index');",
    );
    let graph = graph(&[
        ADMIN_PROVIDER,
        ("routes/admin.php", routes.as_str()),
        ("app/Http/Controllers/VendorController.php", vendor),
        ("app/Http/Controllers/PlainController.php", framework),
    ]);
    assert_eq!(middleware(&graph, "GET /admin/vendor"), ["api"]);
    assert_eq!(middleware(&graph, "GET /admin/plain"), ["api"]);
    let spots = blind_spots(&graph, DYNAMIC);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert_eq!(spots[0].0, "app/Http/Controllers/VendorController.php");
    assert!(
        spots[0].1.contains("Vendor\\Admin\\CrudController"),
        "{spots:?}"
    );
}
