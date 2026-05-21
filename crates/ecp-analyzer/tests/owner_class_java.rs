use ecp_analyzer::java::parser::JavaProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    provider
        .parse_file(Path::new("Test.java"), source.as_bytes())
        .expect("parse_file")
}

#[test]
fn same_method_name_on_different_classes_is_distinguished() {
    let src = "\
class Foo { void run() {} }\n\
class Bar { void run() {} }\n";
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
fn module_level_function_has_no_owner() {
    // Java doesn't have module-level functions; a static method outside
    // any class is not valid. Test a method that is genuinely uncontained
    // by checking a lone class with one method — the class gets None, the
    // method gets the class as owner.
    let src = "class Standalone { void go() {} }\n";
    let g = parse(src);
    let go_node = g.nodes.iter().find(|n| n.name == "go").expect("go");
    assert_eq!(
        go_node.owner_class.as_deref(),
        Some("Standalone"),
        "go must own Standalone; got {:?}",
        go_node.owner_class
    );
    let class_node = g
        .nodes
        .iter()
        .find(|n| n.name == "Standalone")
        .expect("Standalone");
    assert!(
        class_node.owner_class.is_none(),
        "Standalone class must have no owner; got {:?}",
        class_node.owner_class
    );
}
