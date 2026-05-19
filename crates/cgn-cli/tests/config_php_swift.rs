//! Wave 2 — Task F2+F3: PHP `composer.json` + Swift `Package.swift` parsing.
//!
//! Mirrors the inline-test style of the C# F1 config tests in
//! `crates/graph-nexus-cli/src/config_parser.rs`, but lives as an integration
//! test so the public `parse_single_*` entry points stay exercised through
//! the `graph_nexus_cli` crate surface.

use graph_nexus_cli::config_parser::{parse_single_composer_json, parse_single_swift_package};
use std::fs;
use tempfile::TempDir;

// ─── composer.json (Task F2) ─────────────────────────────────────────────────

#[test]
fn composer_extracts_name_and_requires() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("composer.json");
    fs::write(
        &path,
        r#"{
            "name": "vendor/pkg",
            "require": {
                "php": "^8.0",
                "monolog/monolog": "^3.0"
            }
        }"#,
    )
    .unwrap();

    let meta = parse_single_composer_json(&path, dir.path()).expect("composer.json should parse");
    assert_eq!(meta.kind, "composer-json");
    assert_eq!(meta.name.as_deref(), Some("vendor/pkg"));
    assert_eq!(meta.php_version.as_deref(), Some("^8.0"));
    assert!(
        meta.requires.contains(&"monolog/monolog".to_string()),
        "requires must include monolog/monolog, got {:?}",
        meta.requires,
    );
    // The PHP version constraint is captured separately, never as a require key.
    assert!(
        !meta.requires.iter().any(|k| k == "php"),
        "`php` constraint must NOT appear in requires, got {:?}",
        meta.requires,
    );
}

#[test]
fn composer_extracts_dev_requires() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("composer.json");
    fs::write(
        &path,
        r#"{
            "name": "vendor/pkg",
            "require": { "php": "^8.1" },
            "require-dev": {
                "phpunit/phpunit": "^10.0",
                "mockery/mockery": "^1.6"
            }
        }"#,
    )
    .unwrap();

    let meta = parse_single_composer_json(&path, dir.path()).expect("composer.json should parse");
    let mut dev = meta.requires_dev.clone();
    dev.sort();
    assert_eq!(
        dev,
        vec!["mockery/mockery".to_string(), "phpunit/phpunit".to_string()],
        "require-dev keys must be captured",
    );
}

#[test]
fn composer_missing_optional_fields_ok() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("composer.json");
    fs::write(&path, r#"{ "name": "x" }"#).unwrap();

    let meta = parse_single_composer_json(&path, dir.path())
        .expect("minimal composer.json should still parse");
    assert_eq!(meta.kind, "composer-json");
    assert_eq!(meta.name.as_deref(), Some("x"));
    assert!(meta.php_version.is_none());
    assert!(meta.requires.is_empty());
    assert!(meta.requires_dev.is_empty());
}

#[test]
fn composer_malformed_json_returns_none() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("composer.json");
    fs::write(&path, "{ this is not valid json").unwrap();

    assert!(
        parse_single_composer_json(&path, dir.path()).is_none(),
        "malformed composer.json must return None, not panic",
    );
}

// ─── Package.swift (Task F3) ─────────────────────────────────────────────────

#[test]
fn package_swift_extracts_tools_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Package.swift");
    fs::write(
        &path,
        r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(name: "Demo")
"#,
    )
    .unwrap();

    let meta = parse_single_swift_package(&path, dir.path()).expect("Package.swift should parse");
    assert_eq!(meta.kind, "swift-package");
    assert_eq!(meta.tools_version.as_deref(), Some("5.9"));
}

#[test]
fn package_swift_extracts_name() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Package.swift");
    fs::write(
        &path,
        r#"// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "MyPkg",
    products: [],
    dependencies: []
)
"#,
    )
    .unwrap();

    let meta = parse_single_swift_package(&path, dir.path()).expect("Package.swift should parse");
    assert_eq!(meta.name.as_deref(), Some("MyPkg"));
}

#[test]
fn package_swift_extracts_dependency_urls() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Package.swift");
    fs::write(
        &path,
        r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "App",
    dependencies: [
        .package(url: "https://github.com/apple/swift-nio", from: "2.0.0"),
        .package(url: "https://github.com/vapor/vapor", from: "4.0.0"),
        .package(name: "LocalPkg", path: "../LocalPkg")
    ]
)
"#,
    )
    .unwrap();

    let meta = parse_single_swift_package(&path, dir.path()).expect("Package.swift should parse");
    assert_eq!(
        meta.dependency_urls,
        vec![
            "https://github.com/apple/swift-nio".to_string(),
            "https://github.com/vapor/vapor".to_string(),
        ],
        "every .package(url: …) must be captured; .package(path: …) entries are skipped",
    );
}

#[test]
fn package_swift_no_dependencies_ok() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("Package.swift");
    fs::write(
        &path,
        r#"// swift-tools-version:5.9
import PackageDescription

let package = Package(name: "Solo")
"#,
    )
    .unwrap();

    let meta = parse_single_swift_package(&path, dir.path())
        .expect("minimal Package.swift should still parse");
    assert_eq!(meta.kind, "swift-package");
    assert_eq!(meta.name.as_deref(), Some("Solo"));
    assert_eq!(meta.tools_version.as_deref(), Some("5.9"));
    assert!(meta.dependency_urls.is_empty());
}
