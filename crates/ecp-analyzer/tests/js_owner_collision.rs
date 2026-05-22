use ecp_analyzer::javascript::parser::JavaScriptProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use std::path::Path;

fn parse(source: &str) -> LocalGraph {
    let provider = JavaScriptProvider::new().expect("JavaScriptProvider::new");
    provider
        .parse_file(Path::new("test.js"), source.as_bytes())
        .expect("parse_file")
}

fn owners_of(g: &LocalGraph, name: &str) -> Vec<Option<String>> {
    g.nodes
        .iter()
        .filter(|n| n.name == name)
        .map(|n| n.owner_class.clone())
        .collect()
}

/// Two outer functions each contain a nested `function list()`.
/// Before this fix both emitted `owner_class=None` → uid collision.
#[test]
fn nested_function_in_two_outer_functions_has_distinct_owners() {
    let src = "\
function getRoutes() {
    function list(req, res) {
        res.json([]);
    }
    return list;
}

function getHandlers() {
    function list(req, res) {
        res.send('ok');
    }
    return list;
}
";
    let g = parse(src);
    let lists: Vec<_> = g.nodes.iter().filter(|n| n.name == "list").collect();
    assert!(
        lists.len() >= 2,
        "both list() definitions must be emitted: {lists:?}"
    );
    let owners: Vec<_> = lists
        .iter()
        .filter_map(|n| n.owner_class.as_deref())
        .collect();
    assert!(
        owners.contains(&"getRoutes"),
        "one list must have owner getRoutes; owners: {owners:?}"
    );
    assert!(
        owners.contains(&"getHandlers"),
        "one list must have owner getHandlers; owners: {owners:?}"
    );
}

/// A function nested inside an outer function gets `owner_class` set
/// to the outer function name.
#[test]
fn nested_function_owner_is_outer_function_name() {
    let src = "\
function outer() {
    function inner() {}
    return inner;
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
    let src = "function topLevel() {}\n";
    let g = parse(src);
    let owners = owners_of(&g, "topLevel");
    assert!(
        owners.iter().all(|o| o.is_none()),
        "topLevel must have owner_class=None; got {owners:?}"
    );
}

/// Class methods retain their class owner and are not re-stamped.
#[test]
fn class_method_owner_is_class_not_outer_function() {
    let src = "\
class Service {
    handle() {}
}
";
    let g = parse(src);
    let methods: Vec<_> = g.nodes.iter().filter(|n| n.name == "handle").collect();
    assert!(!methods.is_empty(), "handle must be emitted");
    for m in &methods {
        assert_eq!(
            m.owner_class.as_deref(),
            Some("Service"),
            "handle must have owner Service; got {:?}",
            m.owner_class
        );
    }
}
