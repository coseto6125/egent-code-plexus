use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = KotlinProvider::new().expect("KotlinProvider::new");
    provider
        .parse_file(Path::new("test.kt"), source.as_bytes())
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
class Foo { fun run() {} }\n\
class Bar { fun run() {} }\n";
    let g = parse(src);
    let runs: Vec<_> = g.nodes.iter().filter(|n| n.name == "run").collect();
    assert!(
        runs.len() >= 2,
        "both run() methods must be emitted: {runs:?}"
    );
    let owners: Vec<_> = runs
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"Foo"),
        "one run must own Foo; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"Bar"),
        "one run must own Bar; owners: {owners:?}"
    );
}

#[test]
fn top_level_function_has_no_owner() {
    let src = "fun topLevel() {}\n";
    let g = parse(src);
    let oc = owner_of(&g, "topLevel");
    assert!(
        oc.is_none(),
        "topLevel is module-level, owner_class must be None; got {oc:?}"
    );
}
