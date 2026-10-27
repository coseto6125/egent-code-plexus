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
    // Stands for: the two nested functions stay two declarations. Since R8
    // a nested function stores no owner; its lexical parent keeps it apart
    // (a scoped uid) and prints the old owner text as its display owner.
    assert!(fns.iter().all(|n| n.owner_class.is_none()), "{fns:?}");
    assert_eq!(
        display_owners(src, "convertToMutators"),
        ["buildAdminConverter", "buildUserConverter"]
    );
}

/// The display owner of every built node named `name` in `src`, sorted;
/// asserts that no two of them share a uid.
fn display_owners(src: &str, name: &str) -> Vec<String> {
    let mut builder = ecp_analyzer::resolution::builder::GraphBuilder::new();
    builder.add_graph(parse(src));
    let g = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).expect("archive graph");
    let archived =
        rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes)
            .expect("access graph");
    let pool = g.string_pool.as_slice();
    let named: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| g.nodes[i as usize].name.resolve(pool) == name)
        .collect();
    let mut owners: Vec<String> = named
        .iter()
        .map(|&i| archived.display_owner(i).unwrap_or_default().into_owned())
        .collect();
    owners.sort();
    let mut uids: Vec<u64> = named.iter().map(|&i| g.nodes[i as usize].uid).collect();
    uids.sort_unstable();
    uids.dedup();
    assert_eq!(uids.len(), named.len(), "`{name}`: one uid each");
    owners
}

/// A function nested inside an outer TS function prints the outer
/// function name as its display owner (R8: no stored owner).
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
    // Stands for: a reader prints `outer` for `inner`. Since R8 the parser
    // stores no owner (I1) and the display owner walks the lexical parent.
    assert_eq!(inners, [None]);
    assert_eq!(display_owners(src, "inner"), ["outer"]);
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
