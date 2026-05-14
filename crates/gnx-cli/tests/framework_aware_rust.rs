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

    // Expect 2 framework_refs from .route(_, METHOD(handler)):
    //   build_routes  --axum-route-handler-->  login_handler
    //   build_routes  --axum-route-handler-->  logout_handler
    let handler_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "axum-route-handler")
        .collect();
    assert_eq!(
        handler_refs.len(),
        2,
        "expected 2 axum-route-handler refs, got {}: {:?}",
        handler_refs.len(),
        local.framework_refs
    );

    let targets: Vec<&str> = handler_refs
        .iter()
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        targets.contains(&"login_handler"),
        "missing login_handler in {:?}",
        targets
    );
    assert!(
        targets.contains(&"logout_handler"),
        "missing logout_handler in {:?}",
        targets
    );

    // All from enclosing fn build_routes.
    for r in &handler_refs {
        assert_eq!(
            r.source_name, "build_routes",
            "wrong source_name: {}",
            r.source_name
        );
        assert!(
            r.confidence > 0.0 && r.confidence <= 1.0,
            "confidence out of range: {}",
            r.confidence
        );
    }
}
