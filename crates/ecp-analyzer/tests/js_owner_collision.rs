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

fn parse_bindings(source: &str) -> Vec<LocalGraph> {
    use ecp_analyzer::typescript::parser::TypeScriptProvider;
    vec![
        parse(source),
        TypeScriptProvider::new()
            .unwrap()
            .parse_file(Path::new("test.ts"), source.as_bytes())
            .unwrap(),
    ]
}

#[test]
fn test_parse_function_expression_bindings_emits_one_function_each() {
    use ecp_core::graph::NodeKind;
    for graph in parse_bindings(
        "const one = function() {};\nlet two = function named() {};\nvar three = () => {};\nexport const four = function() {};\nconst scalar = 1, five = function() {};\n",
    ) {
        for name in ["one", "two", "three", "four", "five"] {
            let nodes: Vec<_> = graph
                .nodes
                .iter()
                .filter(|node| node.name == name)
                .collect();
            assert_eq!(nodes.len(), 1, "{name}: {nodes:?}");
            assert_eq!(nodes[0].kind, NodeKind::Function);
        }
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.name == "four" && node.is_exported)
        );
        assert!(graph.nodes.iter().any(|node| node.name == "scalar"));
    }
}

#[test]
fn test_build_nested_closure_calls_resolve_lexical_binding() {
    use ecp_analyzer::resolution::builder::GraphBuilder;
    use ecp_core::graph::RelType;
    let source = "
function first() {
  function shared() {
    var step = function() { return 1; };
    var perView = function() { return step(); };
    return perView();
  }
  return shared();
}
function second() {
  function shared() {
    var step = function() { return 2; };
    var perView = function() { return step(); };
    return perView();
  }
  return shared();
}
function outside() { return step(); }
";
    for local in parse_bindings(source) {
        let steps: Vec<_> = local
            .nodes
            .iter()
            .filter(|node| node.name == "step")
            .collect();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].owner_class.as_deref(), Some("first::shared"));
        assert_eq!(steps[1].owner_class.as_deref(), Some("second::shared"));
        let mut builder = GraphBuilder::new();
        builder.add_graph(local);
        let graph = builder.build();
        let pool = &graph.string_pool;
        let mut step_calls = 0;
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.rel_type == RelType::Calls)
        {
            let source = &graph.nodes[edge.source as usize];
            let target = &graph.nodes[edge.target as usize];
            if target.name.resolve(pool) == "step" {
                step_calls += 1;
                assert_eq!(source.name.resolve(pool), "perView");
                assert_eq!(
                    source.owner_class.resolve(pool),
                    target.owner_class.resolve(pool)
                );
            }
            assert_ne!(source.name.resolve(pool), "outside");
        }
        assert_eq!(step_calls, 2);
    }
}

#[test]
fn test_build_hidden_closure_preserves_imported_call() {
    use ecp_analyzer::resolution::builder::GraphBuilder;
    use ecp_core::graph::RelType;
    for local in parse_bindings(
        r#"import { step } from "./dep"; function outer() { const step = function() {}; step(); } function outside() { step(); }"#,
    ) {
        let extension = local
            .file_path
            .extension()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let mut dependency = parse("export function step() {}");
        dependency.file_path = format!("dep.{extension}").into();
        let mut builder = GraphBuilder::new();
        builder.add_graph(local);
        builder.add_graph(dependency);
        let graph = builder.build();
        let pool = &graph.string_pool;
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.rel_type == RelType::Calls
                    && graph.nodes[edge.source as usize].name.resolve(pool) == "outside"
            })
            .expect("outside calls the imported step");
        let target = &graph.nodes[edge.target as usize];
        assert_eq!(target.name.resolve(pool), "step");
        assert_eq!(
            graph.files[target.file_idx as usize].path.resolve(pool),
            format!("dep.{extension}")
        );
    }
}

#[test]
fn test_build_sibling_foreach_callbacks_keep_closure_calls_separate() {
    use ecp_analyzer::resolution::builder::GraphBuilder;
    use ecp_core::graph::RelType;
    let source = "items.forEach(function(item) {\nvar step = function() { return 1; };\nvar perView = function() { return step(); };\nconsume(perView());\n});\nitems.forEach(function(item) {\nvar step = function() { return 2; };\nvar perView = function() { return step(); };\nconsume(perView());\n});\nfunction outside() { return step(); }";
    for local in parse_bindings(source) {
        let owners = owners_of(&local, "step");
        assert_eq!(owners.len(), 2);
        assert!(owners.iter().all(Option::is_some), "{owners:?}");
        assert_ne!(owners[0], owners[1]);
        let mut builder = GraphBuilder::new();
        builder.add_graph(local);
        let graph = builder.build();
        let pool = &graph.string_pool;
        let steps: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.name.resolve(pool) == "step")
            .collect();
        assert_eq!(steps.len(), 2);
        assert_ne!(steps[0].uid, steps[1].uid);
        let mut calls = 0;
        for edge in graph
            .edges
            .iter()
            .filter(|edge| edge.rel_type == RelType::Calls)
        {
            let caller = &graph.nodes[edge.source as usize];
            let callee = &graph.nodes[edge.target as usize];
            if callee.name.resolve(pool) == "step" {
                calls += 1;
                assert_eq!(caller.name.resolve(pool), "perView");
                assert_eq!(
                    caller.owner_class.resolve(pool),
                    callee.owner_class.resolve(pool)
                );
            }
            assert_ne!(caller.name.resolve(pool), "outside");
        }
        assert_eq!(calls, 2);
    }
}
