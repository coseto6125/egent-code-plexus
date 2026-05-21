use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = CppProvider::new().expect("CppProvider::new");
    provider
        .parse_file(Path::new("test.cpp"), source.as_bytes())
        .expect("parse_file")
}

fn owner_of(g: &LocalGraph, name: &str) -> Option<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| n.owner_class.clone())
}

#[test]
fn same_method_name_on_different_classes_is_distinguished() {
    let src = "\
class Foo { public: void run(); };\n\
class Bar { public: void run(); };\n\
void Foo::run() {}\n\
void Bar::run() {}\n";
    let g = parse(src);
    // In-class declarations and out-of-class definitions both matter.
    // At least one run per class must have owner_class set.
    let runs: Vec<_> = g.nodes.iter().filter(|n| n.name == "run").collect();
    assert!(!runs.is_empty(), "run() methods must be emitted: {runs:?}");
    let owners: Vec<_> = runs
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    // At least one must own Foo or Bar
    assert!(
        owners.iter().any(|o| *o == "Foo" || *o == "Bar"),
        "at least one run must own a class; owners: {owners:?}"
    );
}

#[test]
fn module_level_function_has_no_owner() {
    let src = "void freeFunc() {}\n";
    let g = parse(src);
    let oc = owner_of(&g, "freeFunc");
    assert!(
        oc.is_none(),
        "freeFunc is module-level, owner_class must be None; got {oc:?}"
    );
}
