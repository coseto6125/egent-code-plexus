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
