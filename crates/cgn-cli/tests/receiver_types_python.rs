//! Integration test: Python receiver-type binding (P0).
//!
//! When a local variable or parameter has a type annotation,
//! `var.method()` call sites must be rewritten to `Type.method`
//! so the resolver's Tier 2.5 (qualifier-scoped) lookup can pick
//! the correct target instead of falling back to bare-name Tier 3.

use graph_nexus_analyzer::python::PythonProvider;
use graph_nexus_analyzer::resolution::builder::GraphBuilder;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::{NodeKind, RelType};

fn parse(src: &str) -> Vec<RawNode> {
    let provider = PythonProvider::new().unwrap();
    let local = provider
        .parse_file("test.py".as_ref(), src.as_bytes())
        .unwrap();
    local.nodes
}

fn calls_of<'a>(nodes: &'a [RawNode], fn_name: &str) -> &'a [String] {
    nodes
        .iter()
        .find(|n| n.name == fn_name && matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| n.calls.as_slice())
        .unwrap_or(&[])
}

#[test]
fn local_var_annotation_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_local_annotation");
    assert!(
        calls.iter().any(|c| c == "Apple.eat"),
        "expected `Apple.eat` in use_local_annotation calls; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c == "eat"),
        "bare `eat` should be replaced (not duplicated) when type is known; got {:?}",
        calls,
    );
}

#[test]
fn param_annotation_binds_receiver_type() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_param_annotation");
    assert!(
        calls.iter().any(|c| c == "Banana.eat"),
        "expected `Banana.eat` in use_param_annotation calls; got {:?}",
        calls,
    );
}

#[test]
fn unannotated_var_falls_back_to_bare_name() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_no_annotation");
    assert!(
        calls.iter().any(|c| c == "eat"),
        "without annotation, must keep bare `eat` fallback; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c.contains('.')),
        "no qualifier without type info; got {:?}",
        calls,
    );
}

#[test]
fn generic_type_annotation_is_not_bound() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_generic_annotation");
    assert!(
        calls.iter().any(|c| c == "append"),
        "generic `list[Apple]` is not a single identifier — call must stay bare; got {:?}",
        calls,
    );
    assert!(
        !calls.iter().any(|c| c.contains("list.")),
        "must not bind to spurious `list.append`; got {:?}",
        calls,
    );
}

#[test]
fn receiver_bound_call_resolves_to_correct_class_method_e2e() {
    // E2E: parse 3 fixtures (apple.py / banana.py / main.py) → build graph
    // → assert CALLS edge from `use_local_annotation` lands on Apple.eat
    // (not Banana.eat, not unresolved). Multi-file layout mirrors real
    // codebases where same-name methods live in separate class files;
    // Tier 2.5 (qualifier→file) routes `Apple.eat` to apple.py's `eat`.
    let provider = PythonProvider::new().unwrap();
    let apple = provider
        .parse_file(
            "apple.py".as_ref(),
            include_str!("fixtures/receiver_types_apple.py").as_bytes(),
        )
        .unwrap();
    let banana = provider
        .parse_file(
            "banana.py".as_ref(),
            include_str!("fixtures/receiver_types_banana.py").as_bytes(),
        )
        .unwrap();
    let main = provider
        .parse_file(
            "main.py".as_ref(),
            include_str!("fixtures/receiver_types_main.py").as_bytes(),
        )
        .unwrap();
    let mut builder = GraphBuilder::new();
    builder.add_graph(apple);
    builder.add_graph(banana);
    builder.add_graph(main);
    let graph = builder.build();

    let pool = &graph.string_pool;
    let name_of = |s: graph_nexus_core::pool::StrRef| -> &str {
        let start = s.offset as usize;
        std::str::from_utf8(&pool[start..start + s.len as usize]).expect("utf-8 pool")
    };

    let caller_id = graph
        .nodes
        .iter()
        .position(|n| {
            name_of(n.name) == "use_local_annotation"
                && matches!(n.kind, NodeKind::Function | NodeKind::Method)
        })
        .expect("use_local_annotation node missing");

    // Identify the two `eat` methods by their containing file.
    let file_of = |idx: u32| {
        let file_idx = graph.nodes[idx as usize].file_idx as usize;
        let path_ref = graph.files[file_idx].path;
        let start = path_ref.offset as usize;
        std::str::from_utf8(&pool[start..start + path_ref.len as usize]).expect("utf-8 pool")
    };
    let eat_ids: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| name_of(n.name) == "eat")
        .map(|(i, _)| i)
        .collect();
    let apple_eat_id = *eat_ids
        .iter()
        .find(|i| file_of(**i as u32) == "apple.py")
        .expect("Apple.eat (apple.py) missing");
    let banana_eat_id = *eat_ids
        .iter()
        .find(|i| file_of(**i as u32) == "banana.py")
        .expect("Banana.eat (banana.py) missing");

    let calls_edges: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.rel_type == RelType::Calls && e.source == caller_id as u32)
        .collect();

    assert!(
        calls_edges.iter().any(|e| e.target == apple_eat_id as u32),
        "expected CALLS edge → Apple.eat (id {}); got targets {:?}",
        apple_eat_id,
        calls_edges
            .iter()
            .map(|e| (e.target, name_of(graph.nodes[e.target as usize].name)))
            .collect::<Vec<_>>(),
    );
    assert!(
        !calls_edges.iter().any(|e| e.target == banana_eat_id as u32),
        "must NOT emit CALLS edge → Banana.eat (would be hallucination); got targets {:?}",
        calls_edges
            .iter()
            .map(|e| (e.target, name_of(graph.nodes[e.target as usize].name)))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn closure_inherits_outer_scope_type() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "inner");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "nested fn should resolve outer `outer: Apple` via smallest-containing-scope lookup; got {:?}",
        calls,
    );
}

#[test]
fn mixed_annotated_and_bare_receivers_in_same_fn() {
    let src = include_str!("fixtures/receiver_types.py");
    let nodes = parse(src);
    let calls = calls_of(&nodes, "use_mixed");
    assert!(
        calls.iter().any(|c| c == "Apple.peel"),
        "annotated `a.peel()` should bind to Apple.peel; got {:?}",
        calls,
    );
    assert!(
        calls.iter().any(|c| c == "eat"),
        "unannotated `unannotated.eat()` should fall back to bare `eat`; got {:?}",
        calls,
    );
}
