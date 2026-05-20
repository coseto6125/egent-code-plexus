//! Regression: Swift parser must populate `RawNode.decorators` for
//! `@main` / `@available` / etc. on class_declaration. Before this fix,
//! `decorators` was hardcoded `vec![]` so the cross-language entry-point
//! scorer's `@main`-on-struct check never fired — every
//! apple/swift-argument-parser example (the canonical Swift CLI pattern)
//! silently dropped its EntryPoint marker.

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<(String, NodeKind, Vec<String>)> {
    let p = SwiftProvider::new().expect("provider");
    let g = p
        .parse_file(Path::new("test.swift"), src.as_bytes())
        .expect("parse");
    g.nodes
        .into_iter()
        .map(|n| (n.name, n.kind, n.decorators))
        .collect()
}

#[test]
fn at_main_struct_populates_decorators() {
    let nodes = parse("@main\nstruct Repeat: ParsableCommand {\n  func run() {}\n}\n");
    let r = nodes
        .iter()
        .find(|(name, kind, _)| name == "Repeat" && *kind == NodeKind::Struct)
        .expect("Repeat struct missing");
    assert!(
        r.2.iter().any(|d| d == "@main"),
        "expected @main in decorators, got {:?}",
        r.2
    );
}

#[test]
fn at_main_class_populates_decorators() {
    let nodes = parse("@main\nclass AppDelegate {\n  static func main() {}\n}\n");
    let r = nodes
        .iter()
        .find(|(name, kind, _)| name == "AppDelegate" && *kind == NodeKind::Class)
        .expect("AppDelegate class missing");
    assert!(r.2.iter().any(|d| d == "@main"), "decorators: {:?}", r.2);
}

#[test]
fn at_main_enum_populates_decorators() {
    let nodes = parse("@main\nenum AppMain {\n  static func main() {}\n}\n");
    let r = nodes
        .iter()
        .find(|(name, kind, _)| name == "AppMain" && *kind == NodeKind::Enum)
        .expect("AppMain enum missing");
    assert!(r.2.iter().any(|d| d == "@main"), "decorators: {:?}", r.2);
}

#[test]
fn no_attribute_means_empty_decorators() {
    let nodes = parse("struct Plain {}\n");
    let r = nodes
        .iter()
        .find(|(name, _, _)| name == "Plain")
        .expect("Plain struct missing");
    assert!(r.2.is_empty(), "decorators should be empty: {:?}", r.2);
}

#[test]
fn visibility_modifier_still_captures_alongside_attribute() {
    // Regression: the queries.scm change uses alternation+repetition
    // `(modifiers [(visibility_modifier) @export (attribute) @decorator]*)`
    // so a class with BOTH `public` and `@main` must still emit both
    // captures — earlier single-child pattern only captured one.
    let nodes = parse("@main\npublic struct App {}\n");
    let r = nodes
        .iter()
        .find(|(name, _, _)| name == "App")
        .expect("App struct missing");
    assert!(r.2.iter().any(|d| d == "@main"), "decorators: {:?}", r.2);
}
