//! Declaration uids that keep same-named declarations distinct, in C#
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `class Q` in both branches of `#if X` / `#else` |
//! | FO | none: C# has no free functions; method overloads keep their nodes |
//! | M | same-file `partial class P` twice, `A` in the first, `B` in the second |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

const PATH: &str = "P.cs";

fn parse(src: &str) -> LocalGraph {
    CSharpProvider::new()
        .expect("CSharpProvider::new")
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

/// `expected` `(type, member)` pairs, sorted as [`edges`] returns them.
fn sorted(mut expected: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    expected.sort_unstable();
    expected
}

const IF_TWINS: &str = r#"#if X
class Q { void C() { } }
#else
class Q { void D() { } }
#endif
"#;

const PARTIAL_TWICE: &str = r#"partial class P
{
    void A() { }
}
partial class P
{
    void B() { }
}
"#;

/// Stands for: a redefinition keeps its node and membership binds by row
/// (design_uid §2.3, §3 row 5).
#[test]
fn test_build_csharp_if_twin_classes_each_own_their_own_method() {
    let g = build(IF_TWINS);
    let (first, second) = assert_two_kept(&g, NodeKind::Class, "Q", RelType::Defines, None);
    let (c, d) = (callable(&g, "C"), callable(&g, "D"));
    assert_eq!(targets(&g, RelType::HasMethod, first), [c]);
    assert_eq!(targets(&g, RelType::HasMethod, second), [d]);
}

/// Stands for: a same-file partial part keeps its node and owns the
/// members of its own body (design_uid §2.3, §3 rows 5 and 21).
#[test]
fn test_build_csharp_same_file_partial_parts_each_own_their_own_method() {
    let g = build(PARTIAL_TWICE);
    let (first, second) = assert_two_kept(&g, NodeKind::Class, "P", RelType::Defines, None);
    let (a, b) = (callable(&g, "A"), callable(&g, "B"));
    assert_eq!(
        edges(&g, RelType::HasMethod),
        sorted(vec![(first, a), (second, b)])
    );
}
