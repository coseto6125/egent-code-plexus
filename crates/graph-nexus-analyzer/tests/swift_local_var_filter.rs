use graph_nexus_analyzer::swift::parser::SwiftProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::LocalGraph;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = SwiftProvider::new().expect("provider");
    p.parse_file(Path::new("test.swift"), src.as_bytes()).expect("parse")
}

/// Top-level `let` must appear as Variable; function-local `let`/`var`,
/// parameters, and loop variables must NOT appear at all.
#[test]
fn top_level_let_emits_variable() {
    let src = "let topLevel: Int = 42\n";
    let g = parse(src);
    let v = g.nodes.iter().find(|n| n.name == "topLevel");
    assert!(v.is_some(), "topLevel missing from nodes: {:#?}", g.nodes);
    assert_eq!(v.unwrap().kind, NodeKind::Variable, "expected Variable, got {:?}", v.unwrap().kind);
}

#[test]
fn function_local_let_not_emitted() {
    let src = "func doWork() {\n    let local: Int = 1\n}\n";
    let g = parse(src);
    let leaked = g.nodes.iter().find(|n| n.name == "local");
    assert!(
        leaked.is_none(),
        "function-local `let local` leaked into nodes: {:#?}",
        leaked
    );
}

#[test]
fn function_local_var_not_emitted() {
    let src = "func doWork() {\n    var counter = 0\n}\n";
    let g = parse(src);
    let leaked = g.nodes.iter().find(|n| n.name == "counter");
    assert!(
        leaked.is_none(),
        "function-local `var counter` leaked into nodes: {:#?}",
        leaked
    );
}

#[test]
fn loop_variable_not_emitted() {
    let src = "func iter(items: [Int]) {\n    for x in items {}\n}\n";
    let g = parse(src);
    let leaked = g.nodes.iter().find(|n| n.name == "x");
    assert!(
        leaked.is_none(),
        "loop variable `x` leaked into nodes: {:#?}",
        leaked
    );
}

#[test]
fn mixed_top_level_and_local_only_top_emitted() {
    let src = "\
let globalConst: String = \"hello\"

func process() {
    let local = 99
    var tmp = 0
    for i in 0..<10 {}
}
";
    let g = parse(src);

    // Top-level must be present as Variable.
    let global = g.nodes.iter().find(|n| n.name == "globalConst");
    assert!(global.is_some(), "globalConst missing");
    assert_eq!(global.unwrap().kind, NodeKind::Variable);

    // Locals must not appear.
    for leaked in ["local", "tmp", "i"] {
        assert!(
            g.nodes.iter().all(|n| n.name != leaked),
            "`{leaked}` leaked into nodes: {:#?}",
            g.nodes
        );
    }
}
