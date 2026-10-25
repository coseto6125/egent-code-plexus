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
    BS_KIND_OVERRIDE_AMBIGUOUS, BS_KIND_OVERRIDE_BASE_UNRESOLVED,
    BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE, MAX_OVERRIDE_DEPTH, NAME_MATCH_CONFIDENCE,
};
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::contracts::raw_records::Evidence as StoredEvidence;
use ecp_core::graph::{NodeKind, RelType};
use overrides_support::{
    assert_binds_declared_base, assert_evidence, blind_spots, build, graph_of,
    heritage_confidences, override_blind_spots, override_rows, overrides, Evidence,
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
    let spots = override_blind_spots(graph);
    assert!(
        spots.is_empty(),
        "{why}: no override BlindSpot; got {spots:?}"
    );
}

/// Asserts no edge and exactly one BlindSpot of `kind`, in `file` at a row
/// in `rows`, whose hint holds every `needle`. Returns nothing: the kind,
/// place and hint fields are the contract a machine reader consumes.
fn assert_one_blind_spot(
    graph: &ecp_core::graph::ZeroCopyGraph,
    kind: &str,
    file: &str,
    rows: std::ops::RangeInclusive<u32>,
    needles: &[&str],
) {
    assert!(overrides(graph).is_empty(), "{:?}", overrides(graph));
    let spots = override_blind_spots(graph);
    assert_eq!(spots.len(), 1, "one override BlindSpot; got {spots:?}");
    let (got_kind, got_file, row, hint) = &spots[0];
    assert_eq!(
        (got_kind.as_str(), got_file.as_str()),
        (kind, file),
        "{spots:?}"
    );
    assert!(rows.contains(row), "at the overriding method: {spots:?}");
    for needle in needles {
        assert!(hint.contains(needle), "hint holds {needle}: {hint}");
    }
}

#[test]
fn test_overrides_java_empty_file_emits_nothing() {
    let graph = graph_of(&java(), &[("Empty.java", "")]);
    assert_no_override_facts(&graph, "an empty file has no override");
}

/// Stands for: a declared base outside the graph (empty `bases`, set
/// `first_unresolved`) is a reported gap, never a silent "no override".
#[test]
fn test_overrides_java_unresolved_base_reports_blind_spot() {
    let graph = graph_of(
        &java(),
        &[(
            "Child.java",
            "class Child extends Missing {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_one_blind_spot(
        &graph,
        BS_KIND_OVERRIDE_BASE_UNRESOLVED,
        "Child.java",
        1..=2,
        &["`Missing`", "`Child`", "(0 same-named candidates)", "`run`"],
    );
}

/// Stands for: a base whose file is absent from the graph is the same
/// reported gap as an external base.
#[test]
fn test_overrides_java_base_file_absent_from_graph_reports_blind_spot() {
    // The file that declares `b.Base` failed to parse, so the graph holds
    // only the child; the import names a type no file of the graph has.
    let graph = graph_of(
        &java(),
        &[(
            "c/Child.java",
            "package c;\nimport b.Base;\nclass Child extends Base {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_one_blind_spot(
        &graph,
        BS_KIND_OVERRIDE_BASE_UNRESOLVED,
        "c/Child.java",
        3..=4,
        &["`Base`", "(0 same-named candidates)"],
    );
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
    let graph = java_graph(
        "Child.java",
        vec![
            node("Base", NodeKind::Class, (0, 0, 2, 1)),
            node("  ", NodeKind::Method, (1, 2, 1, 10)),
            child,
            blank_override,
        ],
    );
    assert_no_override_facts(&build(&[graph]), "a blank name pairs with nothing");
}

/// Stands for: a blank heritage spelling is no declared base: no edge and
/// no gap.
#[test]
fn test_overrides_java_whitespace_only_base_emits_nothing() {
    let mut child = node("Child", NodeKind::Class, (3, 0, 6, 1));
    child.heritage = vec!["   ".into()];
    let mut run = node("run", NodeKind::Method, (4, 2, 5, 3));
    run.decorators = vec!["@Override".into()];
    let graph = java_graph(
        "Child.java",
        vec![
            node("Base", NodeKind::Class, (0, 0, 2, 1)),
            node("run", NodeKind::Method, (1, 2, 1, 10)),
            child,
            run,
        ],
    );
    assert_no_override_facts(&build(&[graph]), "a blank base pairs with nothing");
}

fn java_graph(path: &str, nodes: Vec<RawNode>) -> LocalGraph {
    LocalGraph {
        file_path: path.into(),
        content_hash: [0; 8],
        nodes,
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
    }
}

// ── Nearest declaration (codex ruling on P7) ───────────────────────────────

/// Stands for: `Overrides` binds the nearest declaration along the class
/// chain — through a base that lacks the method, and never past one that
/// declares it.
#[test]
fn test_overrides_java_grandparent_tostring_binds_nearest() {
    // B lacks toString: C.toString reaches A.toString two levels up.
    let skip = graph_of(
        &java(),
        &[(
            "C.java",
            "class A {\n    public String toString() { return \"a\"; }\n}\nclass B extends A {\n}\nclass C extends B {\n    @Override\n    public String toString() { return \"c\"; }\n}\n",
        )],
    );
    let edges = overrides(&skip);
    assert_eq!(edges.len(), 1, "C.toString -> A.toString; got {edges:?}");
    assert_eq!(edges[0].0.name, "toString");
    assert!(
        (6..=7).contains(&edges[0].0.line),
        "source is C's: {edges:?}"
    );
    assert_eq!(edges[0].1.line, 1, "target is A's toString: {edges:?}");
    assert_evidence(&edges[0], Evidence::Exact);
    assert_eq!(edges[0].2, 1.0, "distance does not lower confidence");

    // B declares toString: C's edge stops at B; B's own edge reaches A.
    let blocked = graph_of(
        &java(),
        &[(
            "C.java",
            "class A {\n    public String toString() { return \"a\"; }\n}\nclass B extends A {\n    @Override\n    public String toString() { return \"b\"; }\n}\nclass C extends B {\n    @Override\n    public String toString() { return \"c\"; }\n}\n",
        )],
    );
    let rows = override_rows(&blocked);
    assert_eq!(rows.len(), 2, "C -> B and B -> A, never C -> A: {rows:?}");
    let (b_to_a, c_to_b) = (rows[0], rows[1]);
    assert!((4..=5).contains(&b_to_a.0) && b_to_a.1 == 1, "{rows:?}");
    assert!(
        (8..=9).contains(&c_to_b.0) && (4..=5).contains(&c_to_b.1),
        "C's edge stops at B: {rows:?}"
    );
}

/// Stands for: an overload in the nearest declaring base is a reported
/// ambiguity, also two levels up, and the lookup never skips past it.
#[test]
fn test_overrides_java_overload_in_grandparent_records_ambiguous_blind_spot() {
    let graph = graph_of(
        &java(),
        &[(
            "C.java",
            "class A {\n    public void run() {}\n    public void run(int n) {}\n}\nclass B extends A {\n}\nclass C extends B {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_one_blind_spot(
        &graph,
        BS_KIND_OVERRIDE_AMBIGUOUS,
        "C.java",
        7..=8,
        &["2 methods named `run`", "`A`"],
    );
}

/// Stands for: a heritage cycle ends the lookup with a reported gap; the
/// pass terminates.
#[test]
fn test_overrides_java_heritage_cycle_records_incomplete_blind_spot() {
    let graph = graph_of(
        &java(),
        &[(
            "C.java",
            "class A extends B {\n}\nclass B extends A {\n}\nclass C extends A {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    assert_one_blind_spot(
        &graph,
        BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE,
        "C.java",
        5..=6,
        &["cycle", "`run`"],
    );
}

/// `C0 extends C1 … extends C{levels}`; only `C{levels}` and `C0` declare
/// `run`, so the nearest declaration is `levels` levels up.
fn chain_source(levels: usize) -> String {
    let mut src =
        String::from("class C0 extends C1 {\n    @Override\n    public void run() {}\n}\n");
    for i in 1..levels {
        src += &format!("class C{i} extends C{} {{\n}}\n", i + 1);
    }
    src += &format!("class C{levels} {{\n    public void run() {{}}\n}}\n");
    src
}

/// Stands for: the lookup climbs up to the depth cap and reports, not
/// guesses, past it.
#[test]
fn test_overrides_java_depth_cap_records_incomplete_blind_spot() {
    let at_cap = graph_of(
        &java(),
        &[("C.java", chain_source(MAX_OVERRIDE_DEPTH).as_str())],
    );
    let edges = overrides(&at_cap);
    assert_eq!(
        edges.len(),
        1,
        "{MAX_OVERRIDE_DEPTH} levels up is in reach: {edges:?}"
    );
    assert!(override_blind_spots(&at_cap).is_empty());

    let past_cap = graph_of(
        &java(),
        &[("C.java", chain_source(MAX_OVERRIDE_DEPTH + 1).as_str())],
    );
    assert_one_blind_spot(
        &past_cap,
        BS_KIND_OVERRIDE_HIERARCHY_INCOMPLETE,
        "C.java",
        1..=2,
        &["deeper than 32 levels", "`run`"],
    );
}

/// Stands for: one declaration reached along two paths (a diamond) is one
/// edge, not two.
#[test]
fn test_overrides_java_interface_diamond_emits_one_edge() {
    let graph = graph_of(
        &java(),
        &[(
            "C.java",
            "interface I {\n    void run();\n}\ninterface J extends I {\n}\ninterface K extends I {\n}\nclass C implements J, K {\n    @Override\n    public void run() {}\n}\n",
        )],
    );
    let rows = override_rows(&graph);
    assert_eq!(rows.len(), 1, "one edge to I.run: {rows:?}");
    assert_eq!(rows[0].1, 1, "{rows:?}");
}

/// Stands for: a base bound by a heuristic resolution never yields an
/// `Exact` edge, even when the method is marked; the edge never claims
/// more than the base's own heritage edge.
#[test]
fn test_overrides_java_heuristic_base_never_exact() {
    let graph = graph_of(
        &java(),
        &[
            (
                "p/Base.java",
                "package p;\npublic class Base {\n    public void run() {}\n}\n",
            ),
            (
                "p/Child.java",
                "package p;\npublic class Child extends Base {\n    @Override\n    public void run() {}\n}\n",
            ),
        ],
    );
    let heritage = heritage_confidences(&graph, "Child");
    assert_eq!(heritage.len(), 1, "{heritage:?}");
    assert!(
        heritage[0] < 1.0,
        "fixture premise: the resolver binds a same-package base below the exact tier; got {heritage:?}"
    );
    let edges = overrides(&graph);
    assert_eq!(edges.len(), 1, "{edges:?}");
    let confidence = edges[0].2;
    assert!(confidence < 1.0 && confidence <= heritage[0], "{edges:?}");
    if heritage[0] <= 0.7 {
        // A unique bare name: the Global tier.
        assert_eq!(
            StoredEvidence::of_stored(confidence, RelType::Overrides),
            StoredEvidence::Heuristic,
            "{edges:?}"
        );
        assert_eq!(confidence, NAME_MATCH_CONFIDENCE);
        assert_eq!(edges[0].3, "post_process:overrides:base-by-name");
    }
}
