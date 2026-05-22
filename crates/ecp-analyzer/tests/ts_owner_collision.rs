use ecp_analyzer::typescript::parser::TypeScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = TypeScriptProvider::new().expect("TypeScriptProvider::new");
    provider
        .parse_file(Path::new("test.ts"), source.as_bytes())
        .expect("parse_file")
}

fn owners_of(g: &LocalGraph, name: &str) -> Vec<Option<String>> {
    g.nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.owner_class.clone())
        .collect()
}

/// Two outer functions each contain a nested `function convertToMutators()`.
/// Before this fix both emitted `owner_class=None` → uid collision.
#[test]
fn nested_function_in_two_outer_functions_has_distinct_owners() {
    let src = "\
function buildUserConverter() {
    function convertToMutators(items: string[]): string[] {
        return items;
    }
    return convertToMutators;
}

function buildAdminConverter() {
    function convertToMutators(items: string[]): string[] {
        return items.map(i => i.toUpperCase());
    }
    return convertToMutators;
}
";
    let g = parse(src);
    let fns: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.name == "convertToMutators")
        .collect();
    assert!(
        fns.len() >= 2,
        "both convertToMutators definitions must be emitted: {fns:?}"
    );
    let owners: Vec<_> = fns
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"buildUserConverter"),
        "one convertToMutators must have owner buildUserConverter; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"buildAdminConverter"),
        "one convertToMutators must have owner buildAdminConverter; owners: {owners:?}"
    );
}

/// A function nested inside an outer TS function gets `owner_class`
/// set to the outer function name.
#[test]
fn nested_function_owner_is_outer_function_name() {
    let src = "\
function outer(): void {
    function inner(): void {}
}
";
    let g = parse(src);
    let inners = owners_of(&g, "inner");
    assert!(!inners.is_empty(), "inner must be emitted");
    assert!(
        inners.iter().any(|o| o.as_deref() == Some("outer")),
        "inner must have owner_class=outer; got {inners:?}"
    );
}

/// Module-level functions must NOT receive an owner from the fn-nesting pass.
#[test]
fn module_level_function_has_no_owner() {
    let src = "function topLevel(): void {}\n";
    let g = parse(src);
    let owners = owners_of(&g, "topLevel");
    assert!(
        owners.iter().all(|o| o.is_none()),
        "topLevel must have owner_class=None; got {owners:?}"
    );
}

/// Class methods retain their class owner and are not re-stamped by
/// the function-nesting pass.
#[test]
fn class_method_owner_is_class_not_outer_function() {
    let src = "\
class OrderService {
    process(): void {}
}
";
    let g = parse(src);
    let methods: Vec<_> = g.nodes.iter().filter(|n| n.name == "process").collect();
    assert!(!methods.is_empty(), "process must be emitted");
    for m in &methods {
        assert_eq!(
            m.owner_class.as_deref(),
            Some("OrderService"),
            "process must have owner OrderService; got {:?}",
            m.owner_class
        );
    }
}

/// NestJS-style: two test files each define `class TestModule`.
/// In a real multi-file scenario these have different `path` in the uid,
/// so they don't collide. This test verifies the single-file case where
/// two `TestModule` classes defined inside different functions get
/// distinct owners.
#[test]
fn nested_class_inside_functions_has_distinct_owners() {
    let src = "\
function setupFirst() {
    class TestModule {}
    return TestModule;
}

function setupSecond() {
    class TestModule {}
    return TestModule;
}
";
    let g = parse(src);
    let mods: Vec<_> = g.nodes.iter().filter(|n| n.name == "TestModule").collect();
    assert!(
        mods.len() >= 2,
        "both TestModule definitions must be emitted: {mods:?}"
    );
    let owners: Vec<_> = mods
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"setupFirst"),
        "one TestModule must have owner setupFirst; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"setupSecond"),
        "one TestModule must have owner setupSecond; owners: {owners:?}"
    );
}
