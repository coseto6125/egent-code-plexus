//! Integration test: Spring framework refs.
use cgn_analyzer::java::JavaProvider;
use cgn_core::analyzer::provider::LanguageProvider;

#[test]
fn spring_autowired_creates_framework_refs() {
    let src = include_str!("fixtures/spring_controller.java");
    let provider = JavaProvider::new().unwrap();
    let local = provider
        .parse_file("Test.java".as_ref(), src.as_bytes())
        .unwrap();

    let autowired: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "spring-autowired")
        .collect();
    assert_eq!(
        autowired.len(),
        2,
        "expected 2 @Autowired refs (UserService + OrderService), got {}: {:?}",
        autowired.len(),
        local.framework_refs
    );

    let pairs: Vec<(&str, &str)> = autowired
        .iter()
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        pairs.contains(&("UserController", "UserService")),
        "missing UserController->UserService in {:?}",
        pairs
    );
    assert!(
        pairs.contains(&("UserController", "OrderService")),
        "missing UserController->OrderService in {:?}",
        pairs
    );

    for r in &autowired {
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
    }
}

#[test]
fn spring_rest_controller_methods_create_framework_refs() {
    let src = include_str!("fixtures/spring_controller.java");
    let provider = JavaProvider::new().unwrap();
    let local = provider
        .parse_file("Test.java".as_ref(), src.as_bytes())
        .unwrap();

    let routes: Vec<_> = local
        .framework_refs
        .iter()
        .filter(|r| r.reason == "spring-route-handler")
        .collect();
    assert_eq!(
        routes.len(),
        2,
        "expected 2 @RestController route methods, got {}: {:?}",
        routes.len(),
        local.framework_refs
    );

    let pairs: Vec<(&str, &str)> = routes
        .iter()
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(pairs.contains(&("UserController", "getUser")));
    assert!(pairs.contains(&("UserController", "createUser")));

    // Negative: notARoute MUST NOT be captured (no @RestController on NotAController).
    assert!(
        !pairs.iter().any(|(_, t)| *t == "notARoute"),
        "notARoute should not be captured: {:?}",
        pairs
    );

    for r in &routes {
        assert!(r.confidence > 0.0 && r.confidence <= 1.0);
    }
}
