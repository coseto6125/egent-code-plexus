//! `Overrides` in Java binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! `@Override` marks the method.

mod overrides_support;

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::post_process::overrides::{
    BS_KIND_NO_OVERRIDE_CONCEPT, BS_KIND_OVERRIDE_AMBIGUOUS,
};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use overrides_support::{
    assert_binds_declared_base, blind_spots, build, graph_of, overrides, Evidence,
};

#[test]
fn test_overrides_java_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &java(),
        ("a/Base.java", "package a;\npublic class Base {\n    public void run() {}\n}\n"),
        ("b/Child.java", "package b;\nclass Base {\n    public void run() {}\n}\nclass Child extends Base {\n    @Override\n    public void run() {}\n}\n"),
        "run",
        Evidence::Exact,
    );
}

// ── Edge list: the inputs around the declared-base rule ────────────────────

fn java() -> JavaProvider {
    JavaProvider::new().unwrap()
}

fn assert_no_override_facts(graph: &ecp_core::graph::ZeroCopyGraph, why: &str) {
    assert!(overrides(graph).is_empty(), "{why}: {:?}", overrides(graph));
    for kind in [BS_KIND_NO_OVERRIDE_CONCEPT, BS_KIND_OVERRIDE_AMBIGUOUS] {
        assert!(blind_spots(graph, kind).is_empty(), "{why}: no {kind}");
    }
}

#[test]
fn test_overrides_java_empty_file_emits_nothing() {
    let graph = graph_of(&java(), &[("Empty.java", "")]);
    assert_no_override_facts(&graph, "an empty file has no override");
}

#[test]
fn test_overrides_java_unresolved_base_emits_nothing() {
    let graph = graph_of(
        &java(),
        &[(
            "Child.java",
            "class Child extends Missing {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_no_override_facts(&graph, "a base outside the graph gives no edge");
}

#[test]
fn test_overrides_java_base_file_absent_from_graph_emits_nothing() {
    // The file that declares `b.Base` failed to parse, so the graph holds
    // only the child; the import names a type no file of the graph has.
    let graph = graph_of(
        &java(),
        &[(
            "c/Child.java",
            "package c;\nimport b.Base;\nclass Child extends Base {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_no_override_facts(&graph, "the base's file is down");
}

#[test]
fn test_overrides_java_class_and_interface_bases_emits_one_edge_each() {
    let graph = graph_of(
        &java(),
        &[(
            "Child.java",
            "class Base {\n    public void run() {}\n}\ninterface Runner {\n    void run();\n}\nclass Child extends Base implements Runner {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    let mut targets: Vec<u32> = overrides(&graph)
        .into_iter()
        .map(|(source, target, _, _)| {
            // Child.run starts at its `@Override` (line 8) or its signature.
            assert_eq!(source.name, "run");
            assert!(source.line >= 7, "{source:?}");
            target.line
        })
        .collect();
    targets.sort_unstable();
    assert_eq!(targets, [1, 4], "Base.run (line 2) and Runner.run (line 5)");
}

#[test]
fn test_overrides_java_overloaded_base_method_records_ambiguous_blind_spot() {
    let graph = graph_of(
        &java(),
        &[(
            "Child.java",
            "class Base {\n    public void run() {}\n    public void run(int n) {}\n}\nclass Child extends Base {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert!(
        overrides(&graph).is_empty(),
        "two `run` overloads: no parameter types to pick one; got {:?}",
        overrides(&graph)
    );
    let spots = blind_spots(&graph, BS_KIND_OVERRIDE_AMBIGUOUS);
    assert_eq!(spots.len(), 1, "{spots:?}");
    assert!((5..=6).contains(&spots[0].1), "at Child.run: {spots:?}");
    assert!(
        spots[0].2.starts_with("2 methods named `run`"),
        "{}",
        spots[0].2
    );
}

fn node(name: &str, kind: NodeKind, span: (u32, u32, u32, u32)) -> RawNode {
    RawNode {
        name: name.into(),
        kind,
        span,
        is_exported: false,
        heritage: vec![],
        type_annotation: None,
        decorators: vec![],
        calls: vec![],
        field_reads: Vec::new(),
        owner_class: None,
        content_hash: 0,
    }
}

#[test]
fn test_overrides_java_whitespace_only_method_names_emit_nothing() {
    let mut child = node("Child", NodeKind::Class, (3, 0, 6, 1));
    child.heritage = vec!["Base".into()];
    let mut blank_override = node("  ", NodeKind::Method, (4, 2, 5, 3));
    blank_override.decorators = vec!["@Override".into()];
    let graph = LocalGraph {
        file_path: "Child.java".into(),
        content_hash: [0; 8],
        nodes: vec![
            node("Base", NodeKind::Class, (0, 0, 2, 1)),
            node("  ", NodeKind::Method, (1, 2, 1, 10)),
            child,
            blank_override,
        ],
        documents: vec![],
        imports: vec![],
        routes: vec![],
        framework_refs: vec![],
        fanout_refs: vec![],
        blind_spots: vec![],
        schema_fields: None,
        event_topics: None,
        tx_scopes: None,
        path_literals: None,
        sql_refs: None,
        call_metas: vec![],
        raw_function_metas: vec![],
        export_aliases: None,
    };
    assert_no_override_facts(&build(&[graph]), "a blank name pairs with nothing");
}
