use ecp_analyzer::resolution::builder::determine_category;
use ecp_core::graph::FileCategory;

// ── Reference: vendor/ ───────────────────────────────────────────────────────

#[test]
fn rust_crates_vendor_routes_to_reference() {
    assert_eq!(
        determine_category("crates/vendor/tree-sitter-cairo/src/parser.c"),
        FileCategory::Reference
    );
}

#[test]
fn go_vendor_routes_to_reference() {
    assert_eq!(
        determine_category("vendor/github.com/stretchr/testify/assert/assertions.go"),
        FileCategory::Reference
    );
}

// ── Reference: node_modules/ ─────────────────────────────────────────────────

#[test]
fn js_node_modules_routes_to_reference() {
    assert_eq!(
        determine_category("frontend/node_modules/react/index.js"),
        FileCategory::Reference
    );
}

#[test]
fn ts_node_modules_routes_to_reference() {
    assert_eq!(
        determine_category("node_modules/@types/node/index.d.ts"),
        FileCategory::Reference
    );
}

// ── Reference: Python venv / site-packages / tox ────────────────────────────

#[test]
fn python_site_packages_routes_to_reference() {
    assert_eq!(
        determine_category(".venv/lib/python3.12/site-packages/requests/__init__.py"),
        FileCategory::Reference
    );
}

#[test]
fn python_venv_routes_to_reference() {
    assert_eq!(
        determine_category("venv/lib/python3.11/site-packages/flask/app.py"),
        FileCategory::Reference
    );
}

#[test]
fn python_dot_venv_routes_to_reference() {
    assert_eq!(
        determine_category("project/.venv/lib/python3.13/site-packages/foo.py"),
        FileCategory::Reference
    );
}

#[test]
fn python_tox_routes_to_reference() {
    assert_eq!(
        determine_category(".tox/py312/lib/python3.12/site-packages/pytest/__init__.py"),
        FileCategory::Reference
    );
}

// ── Reference: Ruby gems / bundle ────────────────────────────────────────────

#[test]
fn ruby_gems_routes_to_reference() {
    assert_eq!(
        determine_category("gems/ruby-2.7.0/gems/rails-7.0.0/lib/rails.rb"),
        FileCategory::Reference
    );
}

#[test]
fn ruby_bundle_routes_to_reference() {
    assert_eq!(
        determine_category(".bundle/ruby/3.0.0/gems/sinatra/lib/sinatra.rb"),
        FileCategory::Reference
    );
}

// ── Reference: Dart pub-cache ────────────────────────────────────────────────

#[test]
fn dart_pub_cache_routes_to_reference() {
    assert_eq!(
        determine_category(".pub-cache/hosted/pub.dev/http-0.13.6/lib/http.dart"),
        FileCategory::Reference
    );
}

// ── Reference: Java .m2 ──────────────────────────────────────────────────────

#[test]
fn java_m2_routes_to_reference() {
    assert_eq!(
        determine_category(".m2/repository/org/springframework/spring-core/5.3.0/spring-core.jar"),
        FileCategory::Reference
    );
}

// ── Reference: Kotlin / Java .gradle ─────────────────────────────────────────

#[test]
fn kotlin_gradle_routes_to_reference() {
    assert_eq!(
        determine_category(
            ".gradle/caches/modules-2/files-2.1/com.google.guava/guava/31.0/guava.jar"
        ),
        FileCategory::Reference
    );
}

// ── Reference: Swift CocoaPods / Carthage / SwiftPM .build ───────────────────

#[test]
fn swift_pods_routes_to_reference() {
    assert_eq!(
        determine_category("Pods/Alamofire/Source/Session.swift"),
        FileCategory::Reference
    );
}

#[test]
fn swift_carthage_routes_to_reference() {
    assert_eq!(
        determine_category("Carthage/Checkouts/Kingfisher/Sources/Cache/ImageCache.swift"),
        FileCategory::Reference
    );
}

#[test]
fn swift_swiftpm_build_routes_to_reference() {
    assert_eq!(
        determine_category(".build/checkouts/swift-argument-parser/Sources/ArgumentParser.swift"),
        FileCategory::Reference
    );
}

// ── Reference: C/C++ third_party / deps ──────────────────────────────────────

#[test]
fn cpp_third_party_routes_to_reference() {
    assert_eq!(
        determine_category("third_party/googletest/include/gtest/gtest.h"),
        FileCategory::Reference
    );
}

#[test]
fn c_deps_routes_to_reference() {
    assert_eq!(
        determine_category("deps/libsodium/src/libsodium/crypto_hash/sha256.c"),
        FileCategory::Reference
    );
}

#[test]
fn cpp_external_routes_to_reference() {
    assert_eq!(
        determine_category("external/abseil-cpp/absl/base/casts.h"),
        FileCategory::Reference
    );
}

// ── Ordering: vendor/…/tests/ routes to Reference, NOT Test ──────────────────

#[test]
fn vendor_tests_subdir_routes_to_reference_not_test() {
    assert_eq!(
        determine_category("vendor/foo/tests/foo_test.go"),
        FileCategory::Reference,
        "vendor path with tests/ subdir must route to Reference, not Test"
    );
}

#[test]
fn node_modules_spec_routes_to_reference_not_test() {
    assert_eq!(
        determine_category("node_modules/chai/lib/chai/interface/assert.spec.js"),
        FileCategory::Reference,
        "node_modules path with .spec. must route to Reference, not Test"
    );
}

// ── Source: normal production paths are NOT Reference ────────────────────────

#[test]
fn production_src_routes_to_source() {
    assert_eq!(determine_category("src/main.rs"), FileCategory::Source);
}

#[test]
fn production_lib_routes_to_source() {
    assert_eq!(
        determine_category("crates/ecp-core/src/graph.rs"),
        FileCategory::Source
    );
}
