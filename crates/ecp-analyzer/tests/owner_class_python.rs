use ecp_analyzer::python::parser::PythonProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = PythonProvider::new().expect("PythonProvider::new");
    provider
        .parse_file(Path::new("test.py"), source.as_bytes())
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
class Foo:\n    def run(self): pass\n\
class Bar:\n    def run(self): pass\n";
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
    let src = "def free_fn(): pass\n";
    let g = parse(src);
    let oc = owner_of(&g, "free_fn");
    assert!(
        oc.is_none(),
        "free_fn is module-level, owner_class must be None; got {oc:?}"
    );
}
