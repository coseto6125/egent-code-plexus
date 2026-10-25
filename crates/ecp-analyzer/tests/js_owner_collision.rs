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
    // Stands for: the two nested functions stay two declarations. Since R8
    // a nested function stores no owner; its lexical parent keeps it apart
    // (a scoped uid) and prints the old owner text as its display owner.
    assert!(lists.iter().all(|n| n.owner_class.is_none()), "{lists:?}");
    assert_eq!(display_owners(src, "list"), ["getHandlers", "getRoutes"]);
}

/// The built graph of `local` and its archived bytes.
fn built(local: LocalGraph) -> (ecp_core::graph::ZeroCopyGraph, rkyv::util::AlignedVec) {
    let mut builder = ecp_analyzer::resolution::builder::GraphBuilder::new();
    builder.add_graph(local);
    let g = builder.build();
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).expect("archive graph");
    (g, bytes)
}

fn access(bytes: &[u8]) -> &ecp_core::graph::ArchivedZeroCopyGraph {
    rkyv::access::<ecp_core::graph::ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes)
        .expect("access graph")
}

/// The display owner of every built node named `name` in `src`, sorted;
/// asserts that no two of them share a uid.
fn display_owners(src: &str, name: &str) -> Vec<String> {
    let (g, bytes) = built(parse(src));
    let archived = access(&bytes);
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

/// A function nested inside an outer function prints the outer function
/// name as its display owner (R8: no stored owner).
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
    // Stands for: a reader prints `outer` for `inner`. Since R8 the parser
    // stores no owner (I1) and the display owner walks the lexical parent.
    assert_eq!(inners, [None]);
    assert_eq!(display_owners(src, "inner"), ["outer"]);
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
        // Stands for: each `step` is placed under its own `shared`. Since R8
        // the parser stores no owner (I1); the display owner prints the
        // path HEAD stored as the owner.
        assert!(steps.iter().all(|s| s.owner_class.is_none()), "{steps:?}");
        let (graph, bytes) = built(local);
        let archived = access(&bytes);
        let pool = &graph.string_pool;
        let mut displayed: Vec<String> = (0..graph.nodes.len() as u32)
            .filter(|&i| graph.nodes[i as usize].name.resolve(pool) == "step")
            .map(|i| archived.display_owner(i).unwrap_or_default().into_owned())
            .collect();
        displayed.sort();
        assert_eq!(displayed, ["first::shared", "second::shared"]);
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
                // The caller and the callee sit in one `shared`.
                assert!(archived.lexical_parent(edge.source).is_some());
                assert_eq!(
                    archived.lexical_parent(edge.source),
                    archived.lexical_parent(edge.target)
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
    use ecp_core::graph::RelType;
    let source = "items.forEach(function(item) {\nvar step = function() { return 1; };\nvar perView = function() { return step(); };\nconsume(perView());\n});\nitems.forEach(function(item) {\nvar step = function() { return 2; };\nvar perView = function() { return step(); };\nconsume(perView());\n});\nfunction outside() { return step(); }";
    for local in parse_bindings(source) {
        // Stands for: the two `step`s sit in two callbacks and stay apart.
        // Since R8 the parser stores no owner (I1); each `step`'s lexical
        // parent is its own callback node, where HEAD stored that
        // callback's name as the owner.
        let owners = owners_of(&local, "step");
        assert_eq!(owners, [None, None]);
        let (graph, bytes) = built(local);
        let archived = access(&bytes);
        let pool = &graph.string_pool;
        let steps: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.name.resolve(pool) == "step")
            .collect();
        assert_eq!(steps.len(), 2);
        assert_ne!(steps[0].uid, steps[1].uid);
        let parents: Vec<Option<u32>> = (0..graph.nodes.len() as u32)
            .filter(|&i| graph.nodes[i as usize].name.resolve(pool) == "step")
            .map(|i| archived.lexical_parent(i))
            .collect();
        assert!(parents.iter().all(Option::is_some), "{parents:?}");
        assert_ne!(parents[0], parents[1]);
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
                // The caller and the callee sit in one callback.
                assert!(archived.lexical_parent(edge.source).is_some());
                assert_eq!(
                    archived.lexical_parent(edge.source),
                    archived.lexical_parent(edge.target)
                );
            }
            assert_ne!(caller.name.resolve(pool), "outside");
        }
        assert_eq!(calls, 2);
    }
}
