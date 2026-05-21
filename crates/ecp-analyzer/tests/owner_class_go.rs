use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = GoProvider::new().expect("GoProvider::new");
    provider
        .parse_file(Path::new("test.go"), source.as_bytes())
        .expect("parse_file")
}

fn owner_of(g: &LocalGraph, name: &str) -> Option<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| n.owner_class.clone())
}

#[test]
fn same_method_name_on_different_receiver_types_is_distinguished() {
    let src = "\
package main\n\
type Foo struct{}\n\
type Bar struct{}\n\
func (f *Foo) Run() {}\n\
func (b *Bar) Run() {}\n";
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
fn module_level_function_has_no_owner() {
    let src = "package main\nfunc FreeFunc() {}\n";
    let g = parse(src);
    let oc = owner_of(&g, "FreeFunc");
    assert!(
        oc.is_none(),
        "FreeFunc is module-level, owner_class must be None; got {oc:?}"
    );
}
