use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use std::path::Path;

fn parse_swift(src: &str) -> ecp_core::analyzer::types::LocalGraph {
    let provider = SwiftProvider::new().expect("SwiftProvider::new");
    provider
        .parse_file(Path::new("test.swift"), src.as_bytes())
        .expect("parse_file")
}

fn kinds(g: &ecp_core::analyzer::types::LocalGraph) -> Vec<&str> {
    g.blind_spots.iter().map(|b| b.kind.as_str()).collect()
}

// ── NSClassFromString: load class by name ──

#[test]
fn swift_nsclass_from_string_emits_blind_spot() {
    let src = r#"
import Foundation
func load(_ name: String) -> AnyClass? {
    return NSClassFromString(name)
}
"#;
    let g = parse_swift(src);
    assert!(
        kinds(&g).contains(&"swift-nsclass-from-string"),
        "expected swift-nsclass-from-string; got: {:?}",
        kinds(&g)
    );
}

// ── perform(Selector(...)) — Objective-C selector dispatch ──

#[test]
fn swift_perform_selector_emits_blind_spot() {
    let src = r#"
import Foundation
class Dispatcher: NSObject {
    func run(_ target: NSObject, _ name: String) {
        target.perform(Selector(name))
    }
}
"#;
    let g = parse_swift(src);
    assert!(
        kinds(&g).contains(&"swift-perform-selector"),
        "expected swift-perform-selector; got: {:?}",
        kinds(&g)
    );
}

// ── unrelated: NOT blind ──

#[test]
fn swift_protocol_method_call_emits_no_blind_spot() {
    let src = r#"
protocol Handler { func handle() }
func run(_ h: Handler) { h.handle() }
"#;
    let g = parse_swift(src);
    assert!(
        g.blind_spots.is_empty(),
        "protocol method dispatch must not emit; got: {:?}",
        g.blind_spots
    );
}

#[test]
fn swift_ordinary_call_emits_no_blind_spot() {
    let src = "func add(_ a: Int, _ b: Int) -> Int { return a + b }\nlet x = add(1, 2)";
    let g = parse_swift(src);
    assert!(
        g.blind_spots.is_empty(),
        "ordinary call must not emit; got: {:?}",
        g.blind_spots
    );
}
