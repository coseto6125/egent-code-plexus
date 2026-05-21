use ecp_analyzer::event_topic::normalize::canonicalize;

#[test]
fn test_event_topic_normalize_comprehensive() {
    // Table-driven test covering all 6 transformations + negative documentation cases.
    let cases = vec![
        // (input, expected_output, description)
        ("", "", "empty string"),
        ("order", "order", "single word unchanged"),
        ("order/created", "order/created", "already canonical"),
        // Rule 1: Environment prefixes stripped
        (
            "prod.order.created",
            "order/created",
            "prod prefix stripped",
        ),
        ("dev.order.created", "order/created", "dev prefix stripped"),
        (
            "staging.order.created",
            "order/created",
            "staging prefix stripped",
        ),
        // Rule 2: Version suffix stripped
        ("order.created.v1", "order/created", "v1 suffix stripped"),
        (
            "order.created.v123",
            "order/created",
            "v123 suffix stripped",
        ),
        ("OrderCreated.v2", "order/created", "version + camel case"),
        // Rule 3: Lowercase
        ("ORDER_CREATED", "order/created", "all uppercase"),
        ("Order_Created", "order/created", "mixed case"),
        // Rule 4: Separator normalization (., _, -, :, /)
        ("order_created", "order/created", "underscore to slash"),
        ("order-created", "order/created", "hyphen to slash"),
        ("order.created", "order/created", "dot to slash"),
        ("order:created", "order/created", "colon to slash"),
        ("order/created", "order/created", "slash unchanged"),
        // Mixed separators
        (
            "order.status-update",
            "order/status/update",
            "mixed separators",
        ),
        (
            "order_status-update",
            "order/status/update",
            "underscore + hyphen",
        ),
        ("order:status.update", "order/status/update", "colon + dot"),
        // Rule 5: Trim leading/trailing /
        ("/order/created", "order/created", "leading slash trimmed"),
        ("order/created/", "order/created", "trailing slash trimmed"),
        ("/order/created/", "order/created", "both slashes trimmed"),
        // Consecutive separators (created by separator replacement)
        ("order..created", "order/created", "consecutive dots"),
        ("order__created", "order/created", "consecutive underscores"),
        ("order--created", "order/created", "consecutive hyphens"),
        // Rule 6: CamelCase to snake_case per segment
        ("OrderCreated", "order/created", "simple camel case"),
        ("userSignedUp", "user/signed/up", "multi-word camel case"),
        ("ProductAdded", "product/added", "camel case two words"),
        (
            "HTTPSConnectionEstablished",
            "https/connection/established",
            "acronym handling: consecutive caps split before Capitalized word",
        ),
        // Combinations: env prefix + version + separators + camel case
        (
            "prod.OrderCreated.v1",
            "order/created",
            "all rules: env + camel + version",
        ),
        (
            "dev.userSignedUp.v2",
            "user/signed/up",
            "all rules: dev + multi-camel + version",
        ),
        (
            "staging.order_Status.v3",
            "order/status",
            "all rules: staging + mixed sep + camel + version",
        ),
        // Negative documentation: region prefixes preserved
        (
            "eu-west-1.order.created",
            "eu/west/1/order/created",
            "region prefix distinct #1",
        ),
        (
            "eu-west-2.order.created",
            "eu/west/2/order/created",
            "region prefix distinct #2",
        ),
        // Negative documentation: tenant IDs preserved
        (
            "tenant-123.order.created",
            "tenant/123/order/created",
            "tenant 123",
        ),
        (
            "tenant-456.order.created",
            "tenant/456/order/created",
            "tenant 456",
        ),
        // Edge cases
        ("ORDER", "order", "single uppercase word"),
        ("CamelCase", "camel/case", "typical camel case"),
        ("camelCase", "camel/case", "starting lowercase camel case"),
        (
            "_leading_underscore",
            "leading/underscore",
            "leading underscore",
        ),
        (
            "trailing_underscore_",
            "trailing/underscore",
            "trailing underscore",
        ),
        // Real-world examples
        (
            "prod.user_PasswordChanged.v3",
            "user/password/changed",
            "real-world: password changed",
        ),
        (
            "dev.OrderShipped.v1",
            "order/shipped",
            "real-world: order shipped",
        ),
        (
            "staging.PaymentProcessed.v2",
            "payment/processed",
            "real-world: payment processed",
        ),
    ];

    for (input, expected, description) in cases {
        let result = canonicalize(input);
        assert_eq!(
            result, expected,
            "canonicalize({:?}) = {:?}, expected {:?} — {}",
            input, result, expected, description
        );
    }
}

#[test]
fn test_hyphen_and_slash_collapse_to_same_canonical() {
    // Negative documentation: both normalize to the same value
    assert_eq!(canonicalize("order-created"), canonicalize("order/created"));
    assert_eq!(canonicalize("order-created"), "order/created");
}

#[test]
fn test_region_prefixes_stay_distinct() {
    // Negative documentation: region prefixes (eu-west-1, eu-west-2) stay distinct
    let eu1 = canonicalize("eu-west-1.order.created");
    let eu2 = canonicalize("eu-west-2.order.created");

    assert_ne!(eu1, eu2, "region prefixes should be distinct");
    assert_eq!(eu1, "eu/west/1/order/created");
    assert_eq!(eu2, "eu/west/2/order/created");
}

#[test]
fn test_tenant_ids_stay_distinct() {
    // Negative documentation: tenant IDs (123, 456) stay distinct
    let t123 = canonicalize("tenant-123.order.created");
    let t456 = canonicalize("tenant-456.order.created");

    assert_ne!(t123, t456, "tenant IDs should be distinct");
    assert_eq!(t123, "tenant/123/order/created");
    assert_eq!(t456, "tenant/456/order/created");
}
