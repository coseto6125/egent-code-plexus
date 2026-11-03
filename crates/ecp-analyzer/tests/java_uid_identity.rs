//! Declaration uids that keep same-named declarations distinct, in Java
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | none: Java has no same-scope redefinition and no conditional compilation |
//! | FO | none: Java has no free functions; method overloads keep their nodes |
//! | M | none: Java declares no type in parts |
//! | enum | `enum E { X }` nested in classes `A` and `B` |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::java::parser::JavaProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

mod uid_chain_support;

const PATH: &str = "M.java";

fn parse(src: &str) -> LocalGraph {
    JavaProvider::new()
        .expect("JavaProvider::new")
        .parse_file(Path::new(PATH), src.as_bytes())
        .expect("parse_file")
}

fn build(src: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(src));
    builder.build()
}

type EdgeRow = (u32, String, u32, String);

/// `(source line, source name, target line, target name)` of every `rel`
/// edge of `merged`, sorted.
fn edge_rows(merged: &MergedGraph<'_>, rel: RelType) -> Vec<EdgeRow> {
    let mut out: Vec<EdgeRow> = merged
        .all_edges()
        .filter(|e| e.rel_type() == rel)
        .map(|e| {
            let (s, t) = (
                merged.node(e.source).unwrap(),
                merged.node(e.target).unwrap(),
            );
            (
                s.start_line(),
                s.name(merged).to_string(),
                t.start_line(),
                t.name(merged).to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The `rel` edges of the full build of `src`, and of the overlay view of
/// the same file over that build, dirty with unchanged content.
fn full_and_overlay(src: &str, rel: RelType) -> (Vec<EdgeRow>, Vec<EdgeRow>) {
    let full = build(src);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&full).expect("archive graph");
    let base =
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).expect("access graph");
    let view = OverlayView::build(base, &[OverlayFileInput::from(parse(src))]).expect("a view");
    (
        edge_rows(&MergedGraph::new(base, None), rel),
        edge_rows(&MergedGraph::new(base, Some(&view)), rel),
    )
}

/// Each enum of `src` defines the variant its own span holds, on the full
/// build and on the overlay: `(enum line, variant line)` of every
/// `Defines` into a node named `variant`, against `expected`.
fn assert_enum_rows(src: &str, variant: &str, expected: &[(u32, u32)]) {
    let (full, overlay) = full_and_overlay(src, RelType::Defines);
    let pick = |rows: &[EdgeRow]| -> Vec<(u32, u32)> {
        rows.iter()
            .filter(|r| r.3 == variant)
            .map(|r| (r.0, r.2))
            .collect()
    };
    assert_eq!(pick(&full), expected, "full build");
    assert_eq!(pick(&overlay), expected, "overlay");
}

const NESTED_ENUMS: &str = r#"class A {
    enum E { X }
}
class B {
    enum E { X }
}
"#;

/// Stands for: enum containment binds by span and row (design_uid §3
/// row 22). Each nested `E` defines the `X` of its own body, never the other
/// one's.
#[test]
fn test_enum_variant_defines_two_mods_same_enum_binds_by_span() {
    assert_enum_rows(NESTED_ENUMS, "X", &[(2, 2), (5, 5)]);
}

/// Stands for: a local class declared in two methods is two nodes, and each
/// keeps its own supertype, so each `get` overrides `Base.get`. Before the
/// fix the later class got no supertypes and its `get` lost its `Overrides`
/// (retrofit `RetrofitTest.java` in the 14-language sample).
#[test]
fn test_build_java_local_class_in_two_methods_each_overrides_base() {
    let src = "class Base { void get() {} }\nclass T {\n  void a() {\n    class L extends Base {\n      @Override void get() {}\n    }\n  }\n  void b() {\n    class L extends Base {\n      @Override void get() {}\n    }\n  }\n}\n";
    let full = build(src);
    let rows = edge_rows(
        &MergedGraph::new(
            rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(
                &rkyv::to_bytes::<rkyv::rancor::Error>(&full).expect("archive graph"),
            )
            .expect("access graph"),
            None,
        ),
        RelType::Overrides,
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().all(|(_, s, _, t)| s == "get" && t == "get"),
        "{rows:?}"
    );
    assert_ne!(rows[0].0, rows[1].0, "{rows:?}");
    assert_eq!(
        rows[0].2, rows[1].2,
        "both override the one Base.get: {rows:?}"
    );
}

const NESTED_TWINS: &str = "class O1 {\n    class H {\n        void run() {}\n    }\n}\nclass O2 {\n    class H {\n        void run() {}\n    }\n}\n";

/// Stands for: N1 and N2 (design_uid §5, phase 2). `O1.H` and `O2.H` are
/// two named nodes with two uids on the full build and the overlay, and each
/// `run` is a member of its own `H` only, with no `overload_set` reference
/// between the two `run`.
#[test]
fn test_build_java_twin_nested_types_two_nodes_own_members() {
    let provider = JavaProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "H", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "run", &[3, 8]);
    uid_chain_support::assert_own_members(
        &g,
        ecp_core::graph::RelType::HasMethod,
        "run",
        &[(3, 2), (8, 7)],
    );
    // `ecp impact H.run@3` lists no caller: nothing calls or references it.
    let run = uid_chain_support::node_at(&g, "run", 3);
    assert_eq!(uid_chain_support::callers(&g, run), 0);
}

const LOCAL_TWINS: &str = "class T {\n    void a() {\n        class L {}\n    }\n    void b() {\n        class L {}\n    }\n}\n";

/// Stands for: L (design_uid §5, phase 2). A local class in each of two
/// methods keys on its own method's anchor: two named `L`, two uids.
#[test]
fn test_build_java_local_class_in_two_methods_two_nodes() {
    uid_chain_support::assert_twins(
        &JavaProvider::new().unwrap(),
        PATH,
        LOCAL_TWINS,
        "L",
        &[3, 6],
    );
}
