use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = CSharpProvider::new().expect("CSharpProvider::new");
    provider
        .parse_file(Path::new("Test.cs"), source.as_bytes())
        .expect("parse_file")
}

#[test]
fn same_method_name_on_different_classes_is_distinguished() {
    let src = "\
class Foo { void Run() {} }\n\
class Bar { void Run() {} }\n";
    let g = parse(src);
    let runs: Vec<_> = g.nodes.iter().filter(|n| n.name == "Run").collect();
    assert!(
        runs.len() >= 2,
        "both Run() methods must be emitted: {runs:?}"
    );
    let owners: Vec<_> = runs
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"Foo"),
        "one Run must own Foo; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"Bar"),
        "one Run must own Bar; owners: {owners:?}"
    );
}

#[test]
fn class_node_has_no_owner() {
    let src = "class Standalone { void Go() {} }\n";
    let g = parse(src);
    let class_node = g
        .nodes
        .iter()
        .find(|n| n.name == "Standalone")
        .expect("Standalone");
    assert!(
        class_node.owner_class.is_none(),
        "Standalone class itself must have no owner; got {:?}",
        class_node.owner_class
    );
}
