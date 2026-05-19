//! AST-pattern framework detection for Swift (UIKit / SwiftUI / Vapor).
//!
//! Ported from upstream `_source_code/gitnexus/src/core/ingestion/languages/swift.ts:281-316`.

use cgn_analyzer::swift::parser::SwiftProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = SwiftProvider::new().unwrap();
    let local = provider
        .parse_file("test.swift".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_framework(refs: &[RawFrameworkRef], framework: &str) -> bool {
    refs.iter().any(|r| r.target_name == framework)
}

#[test]
fn uikit_viewcontroller_emits_ref() {
    let src = r#"
        class MyVC: UIViewController {
            override func viewDidLoad() {
                super.viewDidLoad()
            }
        }
    "#;
    assert!(has_framework(&parse(src), "uikit"));
}

#[test]
fn uikit_ibaction_emits_ref() {
    let src = r#"
        class V {
            @IBOutlet var label: UILabel!
            @IBAction func tap() { }
        }
    "#;
    assert!(has_framework(&parse(src), "uikit"));
}

#[test]
fn swiftui_main_emits_ref() {
    let src = r#"
        @main
        struct MyApp: App {
            var body: some Scene {
                WindowGroup {
                    ContentView()
                }
            }
        }
    "#;
    assert!(has_framework(&parse(src), "swiftui"));
}

#[test]
fn swiftui_property_wrappers_emit_ref() {
    let src = r#"
        struct V: View {
            @StateObject var vm = VM()
            @Published var count = 0
        }
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "swiftui"));
    assert_eq!(
        refs.iter().filter(|r| r.target_name == "swiftui").count(),
        1
    );
}

#[test]
fn vapor_routing_emits_ref() {
    let src = r#"
        import Vapor
        func routes(_ app: Application) {
            app.get("hello") { req in "Hello" }
            app.post("login") { req in req.content.decode(Login.self) }
        }
    "#;
    assert!(has_framework(&parse(src), "vapor"));
}

#[test]
fn no_framework_patterns_no_refs() {
    let src = r#"
        struct Plain {
            func hello() {}
        }
    "#;
    assert!(parse(src).is_empty());
}
