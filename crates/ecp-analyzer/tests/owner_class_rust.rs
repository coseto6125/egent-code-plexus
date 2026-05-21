use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = RustProvider::new().expect("RustProvider::new");
    provider
        .parse_file(Path::new("test.rs"), source.as_bytes())
        .expect("parse_file")
}

fn owner_of(g: &LocalGraph, name: &str) -> Option<String> {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| n.owner_class.clone())
}

#[test]
fn same_method_name_on_different_structs_is_distinguished() {
    let src = r#"
struct Foo;
struct Bar;
impl Foo {
    fn run(&self) {}
}
impl Bar {
    fn run(&self) {}
}
"#;
    let g = parse(src);
    // Both nodes named "run" exist — owner_class differentiates them.
    let runs: Vec<_> = g.nodes.iter().filter(|n| n.name == "run").collect();
    assert_eq!(
        runs.len(),
        2,
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
    let src = r#"
struct Unused;
fn free_fn() {}
"#;
    let g = parse(src);
    let oc = owner_of(&g, "free_fn");
    assert!(
        oc.is_none(),
        "free_fn is module-level, owner_class must be None; got {oc:?}"
    );
}

#[test]
fn impl_method_owner_matches_struct() {
    let src = r#"
struct Calc;
impl Calc {
    fn add(&self) -> i32 { 0 }
}
"#;
    let g = parse(src);
    let oc = owner_of(&g, "add");
    assert_eq!(oc.as_deref(), Some("Calc"), "add must own Calc; got {oc:?}");
}
