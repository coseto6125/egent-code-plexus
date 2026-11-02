//! Declaration uids that keep same-named declarations distinct, in Python
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `def f` twice at module level; `class C` twice |
//! | FO | none: Python has no overloads |
//! | M | none: Python declares no type in parts |
//! | merge | `x = 1; x = 2`: one entity |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::python::parser::PythonProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

const PATH: &str = "m.py";

fn parse(src: &str) -> LocalGraph {
    PythonProvider::new()
        .expect("PythonProvider::new")
        .parse_file(Path::new(PATH), src.as_bytes())
        .expect("parse_file")
}

fn build(src: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(src));
    builder.build()
}

/// The nodes of `kind` named `name`, in source order.
fn named(g: &ZeroCopyGraph, kind: NodeKind, name: &str) -> Vec<u32> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| {
            let n = &g.nodes[i as usize];
            n.kind == kind && n.name.resolve(pool) == name
        })
        .collect();
    out.sort_by_key(|&i| g.nodes[i as usize].span);
    out
}

/// The one callable named `name`. A `Process` node takes its entry
/// function's name, so only a Function or Method counts.
fn callable(g: &ZeroCopyGraph, name: &str) -> u32 {
    let pool = g.string_pool.as_slice();
    let hits: Vec<u32> = (0..g.nodes.len() as u32)
        .filter(|&i| {
            let n = &g.nodes[i as usize];
            matches!(n.kind, NodeKind::Function | NodeKind::Method) && n.name.resolve(pool) == name
        })
        .collect();
    assert_eq!(hits.len(), 1, "setup: one callable `{name}`");
    hits[0]
}

/// `(source, target)` of every `rel` edge, sorted.
fn edges(g: &ZeroCopyGraph, rel: RelType) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel)
        .map(|e| (e.source, e.target))
        .collect();
    out.sort_unstable();
    out
}

fn sources(g: &ZeroCopyGraph, rel: RelType, target: u32) -> Vec<u32> {
    edges(g, rel)
        .into_iter()
        .filter(|&(_, t)| t == target)
        .map(|(s, _)| s)
        .collect()
}

fn targets(g: &ZeroCopyGraph, rel: RelType, source: u32) -> Vec<u32> {
    edges(g, rel)
        .into_iter()
        .filter(|&(s, _)| s == source)
        .map(|(_, t)| t)
        .collect()
}

fn uid_collisions(g: &ZeroCopyGraph) -> usize {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .filter(|b| b.kind.resolve(pool) == "uid-collision")
        .count()
}

/// Two declarations `name` of `kind`: two named nodes with distinct uids,
/// each with exactly one `parent` edge (`Defines`, or the membership edge
/// of a member), no `uid-collision` BlindSpot, and, for `calls = Some((a,
/// b))` where the first body calls `a` and the second `b`, each body's
/// `Calls` starting at its own node. Returns the two nodes.
fn assert_two_kept(
    g: &ZeroCopyGraph,
    kind: NodeKind,
    name: &str,
    parent: RelType,
    calls: Option<(&str, &str)>,
) -> (u32, u32) {
    let decls = named(g, kind, name);
    assert_eq!(decls.len(), 2, "two named `{name}` nodes");
    let (first, second) = (decls[0], decls[1]);
    assert_ne!(g.nodes[first as usize].uid, g.nodes[second as usize].uid);
    for decl in [first, second] {
        assert_eq!(
            sources(g, parent, decl).len(),
            1,
            "one {parent:?} into each"
        );
    }
    assert_eq!(uid_collisions(g), 0);
    if let Some((a_name, b_name)) = calls {
        let (a, b) = (callable(g, a_name), callable(g, b_name));
        let (from_first, from_second) = (
            targets(g, RelType::Calls, first),
            targets(g, RelType::Calls, second),
        );
        assert!(from_first.contains(&a), "the first body calls {a_name}");
        assert!(
            !from_first.contains(&b),
            "the first body never calls {b_name}"
        );
        assert!(from_second.contains(&b), "the second body calls {b_name}");
        assert!(
            !from_second.contains(&a),
            "the second body never calls {a_name}"
        );
    }
    (first, second)
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

/// `expected` `(type, member)` pairs, sorted as [`edges`] returns them.
fn sorted(mut expected: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    expected.sort_unstable();
    expected
}

const DEF_TWICE: &str = r#"def a():
    return 1


def b():
    return 2


def f():
    return a()


def f():
    return b()
"#;

const CLASS_TWICE: &str = r#"class C:
    def a(self):
        return 1


class C:
    def b(self):
        return 2
"#;

const ASSIGNED_TWICE: &str = "x = 1\nx = 2\n";

#[test]
fn test_build_python_def_twice_keeps_two_nodes_with_own_calls() {
    let g = build(DEF_TWICE);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

/// Stands for: membership binds by row (design_uid §3 row 5).
/// `HasMethod C@1 -> a`, `C@2 -> b`, nothing else.
#[test]
fn test_build_python_class_twice_each_owns_its_own_method() {
    let g = build(CLASS_TWICE);
    let (first, second) = assert_two_kept(&g, NodeKind::Class, "C", RelType::Defines, None);
    let (a, b) = (callable(&g, "a"), callable(&g, "b"));
    assert_eq!(
        edges(&g, RelType::HasMethod),
        sorted(vec![(first, a), (second, b)])
    );
}

/// Stands for: a merged row maps to its survivor and gives its edge once
/// (design_uid §3 row 4). Exactly one `File -> x`, on the full build and on
/// the overlay.
#[test]
fn test_build_python_variable_assigned_twice_defines_once() {
    let g = build(ASSIGNED_TWICE);
    let x = named(&g, NodeKind::Variable, "x");
    assert_eq!(x.len(), 1, "one named `x`");
    assert_eq!(sources(&g, RelType::Defines, x[0]).len(), 1);
    assert_eq!(uid_collisions(&g), 0);
    let (full, overlay) = full_and_overlay(ASSIGNED_TWICE, RelType::Defines);
    let expected = vec![(1, PATH.to_string(), 1, "x".to_string())];
    assert_eq!(full, expected, "full build");
    assert_eq!(overlay, expected, "overlay");
}
