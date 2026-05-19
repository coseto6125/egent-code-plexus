//! Framework detection (Laravel) for the PHP parser.
//!
//! Ported from upstream `gitnexus/src/core/group/extractors/http-patterns/php.ts`.
//! Laravel route detection is gated by the `Illuminate` use statement;
//! bare `Route::` in a non-Laravel codebase must NOT surface as a route.

use cgn_analyzer::php::parser::PhpProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = PhpProvider::new().unwrap();
    let local = provider
        .parse_file("test.php".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_ref(refs: &[RawFrameworkRef], target: &str, reason: &str) -> bool {
    refs.iter()
        .any(|r| r.target_name == target && r.reason == reason)
}

#[test]
fn laravel_route_controller_action_emits_framework_ref() {
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;
        Route::get('/users', [UserController::class, 'index']);
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "UserController@index", "laravel-route"),
        "expected laravel-route ref UserController@index, got: {:?}",
        refs
    );
}

#[test]
fn laravel_route_closure_emits_anonymous_ref() {
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;
        Route::get('/users', function () { return User::all(); });
    "#;
    let refs = parse(src);
    assert!(
        has_ref(&refs, "<anonymous>", "laravel-route"),
        "expected laravel-route ref with <anonymous> target, got: {:?}",
        refs
    );
}

#[test]
fn laravel_route_post_method_emits_framework_ref() {
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;
        Route::post('/users', [UserController::class, 'store']);
    "#;
    let refs = parse(src);
    assert!(has_ref(&refs, "UserController@store", "laravel-route"));
}

#[test]
fn laravel_no_import_no_ref() {
    // `Route::get(...)` without `use Illuminate\...` must NOT emit a
    // laravel-route — the bare class name `Route` could come from anywhere.
    let src = r#"<?php
        Route::get('/users', [UserController::class, 'index']);
    "#;
    let refs = parse(src);
    assert!(
        !refs.iter().any(|r| r.reason == "laravel-route"),
        "must not emit laravel-route without Illuminate import, got: {:?}",
        refs
    );
}

#[test]
fn laravel_multiple_routes_emit_distinct_refs() {
    let src = r#"<?php
        use Illuminate\Support\Facades\Route;
        Route::get('/users', [UserController::class, 'index']);
        Route::post('/users', [UserController::class, 'store']);
        Route::get('/posts', [PostController::class, 'index']);
    "#;
    let refs = parse(src);
    assert!(has_ref(&refs, "UserController@index", "laravel-route"));
    assert!(has_ref(&refs, "UserController@store", "laravel-route"));
    assert!(has_ref(&refs, "PostController@index", "laravel-route"));
}
