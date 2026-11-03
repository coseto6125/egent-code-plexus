//! Declaration uids that keep same-named declarations distinct, in Kotlin
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `fun f()` twice |
//! | FO | `fun f(x: Int)` and `fun f(x: String)` at the top level |
//! | M | none: Kotlin declares no type in parts |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

mod uid_chain_support;

const PATH: &str = "m.kt";

fn parse(src: &str) -> LocalGraph {
    KotlinProvider::new()
        .expect("KotlinProvider::new")
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

const FUN_TWICE: &str = r#"fun a(): Int = 1
fun b(): Int = 2
fun f(): Int = a()
fun f(): Int = b()
"#;

const OVERLOADS: &str = r#"fun a(): Int = 1
fun b(): Int = 2
fun f(x: Int): Int = a()
fun f(x: String): Int = b()
"#;

#[test]
fn test_build_kotlin_fun_twice_keeps_two_nodes_with_own_calls() {
    let g = build(FUN_TWICE);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

#[test]
fn test_build_kotlin_top_level_overloads_keep_two_nodes_with_own_calls() {
    let g = build(OVERLOADS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

const NESTED_TWINS: &str = "class O1 {\n    class H {\n        fun run() {}\n    }\n}\nclass O2 {\n    class H {\n        fun run() {}\n    }\n}\n";

/// Stands for: N1 and N2 (design_uid §5, phase 2). `O1.H` and `O2.H` are
/// two named nodes with two uids on the full build and the overlay, and each
/// `run` is a member of its own `H` only, with no `overload_set` reference
/// between the two `run`.
#[test]
fn test_build_kotlin_twin_nested_types_two_nodes_own_members() {
    let provider = KotlinProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "H", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "run", &[3, 8]);
    uid_chain_support::assert_own_members(
        &g,
        ecp_core::graph::RelType::HasMethod,
        "run",
        &[(3, 2), (8, 7)],
    );
}

const DEEP_TWINS: &str = "class A {\n    class B {\n        class H\n    }\n}\nclass C {\n    class B {\n        class H\n    }\n}\n";

/// Stands for: N2 at depth 2. `ecp find H --file kt_deep.kt` finds two
/// nodes: `A.B.H` and `C.B.H` chain on two different `B` anchors.
#[test]
fn test_build_kotlin_depth_two_twin_types_find_two() {
    let provider = KotlinProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, "kt_deep.kt", DEEP_TWINS, "B", &[2, 7]);
    uid_chain_support::assert_twins(&provider, "kt_deep.kt", DEEP_TWINS, "H", &[3, 8]);
}

const LOCAL_TWINS: &str = "fun a() {\n    fun l() {}\n}\nfun b() {\n    fun l() {}\n}\n";

/// Stands for: L (design_uid §5, phase 2). A local `fun l` in each of two
/// functions is two named nodes with two uids.
#[test]
fn test_build_kotlin_local_fun_in_two_functions_two_nodes() {
    uid_chain_support::assert_twins(
        &KotlinProvider::new().unwrap(),
        PATH,
        LOCAL_TWINS,
        "l",
        &[2, 5],
    );
}
