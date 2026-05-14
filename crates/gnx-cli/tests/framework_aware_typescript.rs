//! Integration test: TypeScript Express framework refs (T4).
use gnx_analyzer::typescript::TypeScriptProvider;
use gnx_core::analyzer::provider::LanguageProvider;

#[test]
fn express_route_creates_framework_refs_for_handlers() {
    let src = include_str!("fixtures/express_app.ts");
    let provider = TypeScriptProvider::new().unwrap();
    let local = provider
        .parse_file("test.ts".as_ref(), src.as_bytes())
        .unwrap();

    // Expect 2 framework_refs from app.METHOD(path, handler):
    //   <module-level> --express-route-handler-->  loginHandler
    //   <module-level> --express-route-handler-->  logoutHandler
    let handler_refs: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "express-route-handler")
        .collect();
    assert_eq!(
        handler_refs.len(),
        2,
        "expected 2 express-route-handler refs, got {}: {:?}",
        handler_refs.len(),
        local.framework_refs
    );

    let targets: Vec<&str> = handler_refs
        .iter()
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        targets.contains(&"loginHandler"),
        "missing loginHandler in {:?}",
        targets
    );
    assert!(
        targets.contains(&"logoutHandler"),
        "missing logoutHandler in {:?}",
        targets
    );

    for r in &handler_refs {
        assert!(
            r.confidence > 0.0 && r.confidence <= 1.0,
            "confidence out of range: {}",
            r.confidence
        );
    }
}
