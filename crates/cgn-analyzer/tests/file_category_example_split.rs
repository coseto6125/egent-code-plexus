//! Regression: `/examples/` / `/sample/` / `/demo/` paths must classify as
//! `FileCategory::Example` (not `Test`), so that route / handler emission
//! reaches them. Framework example apps (Express's `examples/auth/`,
//! Flask's `examples/tutorial/`, NestJS's `sample/`) are canonical "how to
//! wire routes" references that LLM consumers want to navigate.
//!
//! Round 80: previously `is_test` blanket-grouped `/examples/` with `/tests/`
//! and `builder.rs:332-340` skipped both — `gnx-rs` emitted zero Routes for
//! the entire `.sample_repo/JavaScript/examples/` corpus (82 ref-side rows).
use cgn_analyzer::resolution::builder::determine_category;
use cgn_core::graph::FileCategory;

#[test]
fn examples_path_classifies_as_example() {
    assert_eq!(
        determine_category("examples/auth/index.js"),
        FileCategory::Example,
    );
    assert_eq!(
        determine_category("packages/foo/examples/demo/main.ts"),
        FileCategory::Example,
    );
}

#[test]
fn sample_path_classifies_as_example() {
    assert_eq!(
        determine_category("sample/users-app/users.controller.ts"),
        FileCategory::Example,
    );
    assert_eq!(
        determine_category("samples/01-cats-app/src/main.ts"),
        FileCategory::Example,
    );
}

#[test]
fn demo_path_classifies_as_example() {
    assert_eq!(
        determine_category("demo/serve.py"),
        FileCategory::Example,
    );
    assert_eq!(
        determine_category("packages/x/demos/basic/main.go"),
        FileCategory::Example,
    );
}

#[test]
fn tests_path_still_classifies_as_test() {
    // Test fixtures (`@app.route('/test_setup')`, `_test.go` files) keep
    // Test classification — they remain route-skipped to avoid polluting
    // the production-route surface.
    assert_eq!(
        determine_category("tests/test_routing.py"),
        FileCategory::Test,
    );
    assert_eq!(
        determine_category("packages/core/test/router.spec.ts"),
        FileCategory::Test,
    );
    assert_eq!(
        determine_category("src/server_test.go"),
        FileCategory::Test,
    );
}

#[test]
fn example_takes_precedence_over_test_substring_collision() {
    // `examples/test_helpers.js` — the `/examples/` segment wins over the
    // `/test_` substring; the file is part of an example app and should
    // surface for users browsing that example.
    assert_eq!(
        determine_category("examples/test_helpers/index.js"),
        FileCategory::Example,
    );
}

#[test]
fn reference_path_still_wins_over_example() {
    // Vendor / node_modules etc. classify as Reference first — those are
    // genuinely third-party and shouldn't surface as user-facing examples.
    assert_eq!(
        determine_category("node_modules/express/examples/auth/index.js"),
        FileCategory::Reference,
    );
    assert_eq!(
        determine_category("vendor/foo/examples/x.go"),
        FileCategory::Reference,
    );
}

#[test]
fn plain_source_unaffected() {
    assert_eq!(
        determine_category("src/lib.rs"),
        FileCategory::Source,
    );
    assert_eq!(
        determine_category("packages/core/router.ts"),
        FileCategory::Source,
    );
}
