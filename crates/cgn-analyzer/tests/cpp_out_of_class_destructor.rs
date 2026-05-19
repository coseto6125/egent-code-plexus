//! C++ out-of-class destructor definitions — `ClassName::~ClassName() { ... }`
//! — must emit as Method. tree-sitter-cpp parses them as
//! `function_declarator > qualified_identifier > destructor_name`, but the
//! existing method query only enumerated `identifier` and `field_identifier`
//! inside the qualified_identifier alternation. The `~ClassName` slot was
//! silently dropped.
//!
//! Regression for Round 69: 5 destructor entries in the Cpp Method real_ref
//! bucket after the .h dispatch + header guard fixes (Rounds 64–65).

use cgn_analyzer::cpp::parser::CppProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CppProvider::new().expect("CppProvider init");
    p.parse_file(Path::new("t.cpp"), src.as_bytes()).expect("parse_file")
}

fn methods(g: &LocalGraph) -> Vec<&str> {
    g.nodes.iter().filter(|n| n.kind == NodeKind::Method).map(|n| n.name.as_str()).collect()
}

#[test]
fn out_of_class_destructor_emits_method() {
    let g = parse("Foo::~Foo() {}\n");
    assert!(methods(&g).contains(&"~Foo"));
}

#[test]
fn out_of_class_destructor_with_body_emits_method() {
    let g = parse("Fuzzer::~Fuzzer() { delete[] data_; }\n");
    assert!(methods(&g).contains(&"~Fuzzer"));
}

#[test]
fn out_of_class_destructor_eq_default_emits_method() {
    // Common real-corpus pattern from nlohmann/json regression suites and
    // Fuzzer: `Foo::~Foo() = default;`. The `= default` clause is part of
    // the function_definition shape (special-member default), not a body.
    let g = parse("ParserImpl::~ParserImpl() = default;\n");
    assert!(methods(&g).contains(&"~ParserImpl"));
}

#[test]
fn in_class_destructor_still_emits_method() {
    // Regression guard: the existing in-class destructor path must keep
    // working. That goes through a separate (destructor_name) capture
    // branch, not the qualified_identifier alternation.
    let g = parse("class Foo {\n    ~Foo() {}\n};\n");
    assert!(methods(&g).contains(&"~Foo"));
}

#[test]
fn out_of_class_regular_method_still_emits() {
    // Regression guard: the qualified_identifier name alternation now
    // includes destructor_name in addition to identifier/field_identifier;
    // the latter two must still match plain `Foo::bar()`.
    let g = parse("void Foo::bar() {}\nint Foo::baz() const { return 0; }\n");
    let ms = methods(&g);
    assert!(ms.contains(&"bar"), "bar must emit: {ms:?}");
    assert!(ms.contains(&"baz"), "baz must emit: {ms:?}");
}
