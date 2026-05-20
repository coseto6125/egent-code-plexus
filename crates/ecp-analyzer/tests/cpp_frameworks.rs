//! AST-pattern framework detection for C++ (Qt).
//!
//! Ported from upstream `_source_code/gitnexus/src/core/ingestion/languages/c-cpp.ts:414-431`.
//! Upstream's `cProvider` has no `astFrameworkPatterns`, so this is C++-only.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawFrameworkRef;

fn parse(src: &str) -> Vec<RawFrameworkRef> {
    let provider = CppProvider::new().unwrap();
    let local = provider
        .parse_file("test.cpp".as_ref(), src.as_bytes())
        .unwrap();
    local.framework_refs
}

fn has_framework(refs: &[RawFrameworkRef], framework: &str) -> bool {
    refs.iter().any(|r| r.target_name == framework)
}

#[test]
fn qt_q_object_macro_emits_ref() {
    let src = r#"
        class MyWidget : public QWidget {
            Q_OBJECT
        public:
            Q_INVOKABLE void run();
        };
    "#;
    let refs = parse(src);
    assert!(has_framework(&refs, "qt"));
    assert_eq!(refs.iter().filter(|r| r.target_name == "qt").count(), 1);
}

#[test]
fn qt_property_emits_ref() {
    let src = r#"
        class C {
            Q_PROPERTY(int count READ count)
        };
    "#;
    assert!(has_framework(&parse(src), "qt"));
}

#[test]
fn qt_application_main_emits_ref() {
    let src = r#"
        int main(int argc, char** argv) {
            QApplication app(argc, argv);
            return app.exec();
        }
    "#;
    assert!(has_framework(&parse(src), "qt"));
}

#[test]
fn no_qt_patterns_no_refs() {
    let src = r#"
        #include <iostream>
        int main() {
            std::cout << "hello";
            return 0;
        }
    "#;
    assert!(parse(src).is_empty());
}
