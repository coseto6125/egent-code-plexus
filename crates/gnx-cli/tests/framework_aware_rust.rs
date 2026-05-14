//! Integration test: Rust Axum framework refs (T3).
use gnx_analyzer::rust::parser::RustProvider;
use gnx_core::analyzer::provider::LanguageProvider;

#[test]
fn axum_route_creates_framework_refs_for_handlers() {
    let src = include_str!("fixtures/axum_router.rs.txt");
    let provider = RustProvider::new().unwrap();
    let local = provider
        .parse_file("test.rs".as_ref(), src.as_bytes())
        .unwrap();

    // Expect 3 framework_refs from .route(_, METHOD(handler)):
    //   build_routes  --axum-route-handler-->  login_handler
    //   build_routes  --axum-route-handler-->  logout_handler
    //   main          --axum-route-handler-->  root_handler   (regression: fn main() with no return type)
    let handler_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "axum-route-handler")
        .collect();
    assert_eq!(
        handler_refs.len(),
        3,
        "expected 3 axum-route-handler refs, got {}: {:?}",
        handler_refs.len(),
        local.framework_refs
    );

    let pairs: Vec<(&str, &str)> = handler_refs
        .iter()
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        pairs.contains(&("build_routes", "login_handler")),
        "missing build_routes→login_handler in {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("build_routes", "logout_handler")),
        "missing build_routes→logout_handler in {:?}",
        pairs
    );
    // Regression guard: `fn main()` (no return type) must be extracted so
    // its framework_ref isn't silently dropped (rust/queries.scm bug).
    assert!(
        pairs.contains(&("main", "root_handler")),
        "missing main→root_handler — fn main() without return type was not extracted; \
         check rust/queries.scm's `return_type: (_)?` modifier. Got: {:?}",
        pairs
    );

    for r in &handler_refs {
        assert!(
            r.confidence > 0.0 && r.confidence <= 1.0,
            "confidence out of range: {}",
            r.confidence
        );
    }
}
