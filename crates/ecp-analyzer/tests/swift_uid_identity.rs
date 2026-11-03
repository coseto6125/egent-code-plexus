//! Declaration uids that keep same-named declarations distinct, in Swift
//! (C1-identity.md "Collision shapes per language", rows R / FO / M). A
//! redefinition of a kind that keeps its own node is a second named node with
//! its own uid, its own parent edge and its own calls; membership and
//! containment edges bind the row that declares them; a merged repeat gives
//! each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | none of its own: a same-signature repeat parses as the FO rows |
//! | FO | top-level `func f(_: Int)` and `func f(_: String)` |
//! | M | `func m` in two `extension K` blocks |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

use crate::uid_chain_support;

const PATH: &str = "m.swift";

fn parse(src: &str) -> LocalGraph {
    SwiftProvider::new()
        .expect("SwiftProvider::new")
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

const OVERLOADS: &str = r#"func a() -> Int { return 1 }
func b() -> Int { return 2 }
func f(_ x: Int) -> Int { return a() }
func f(_ x: String) -> Int { return b() }
"#;

const TWO_EXTENSIONS: &str = r#"struct K {}
extension K {
    func m() -> Int { return a() }
}
extension K {
    func m() -> Int { return b() }
}
func a() -> Int { return 1 }
func b() -> Int { return 2 }
"#;

#[test]
fn test_build_swift_top_level_overloads_keep_two_nodes_with_own_calls() {
    let g = build(OVERLOADS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

/// Stands for: membership binds by row (C1-identity.md "Edge sources bind by
/// row"). The struct owns the `m` of each extension, through the owner pass.
#[test]
fn test_build_swift_method_in_two_extensions_each_owned_by_the_type() {
    let g = build(TWO_EXTENSIONS);
    let (first, second) = assert_two_kept(
        &g,
        NodeKind::Method,
        "m",
        RelType::HasMethod,
        Some(("a", "b")),
    );
    let k = named(&g, NodeKind::Struct, "K");
    assert_eq!(sources(&g, RelType::HasMethod, first), k);
    assert_eq!(sources(&g, RelType::HasMethod, second), k);
}

const NESTED_TWINS: &str = "class O1 {\n    class H {\n        func run() {}\n    }\n}\nclass O2 {\n    class H {\n        func run() {}\n    }\n}\n";

/// Stands for: N1 and N2 (C1-identity.md "Collision shapes per language").
/// `O1.H` and `O2.H` are two named nodes with two uids on the full build and
/// the overlay, and each `run` is a member of its own `H` only, with no
/// `overload_set` reference between the two `run`.
#[test]
fn test_build_swift_twin_nested_types_two_nodes_own_members() {
    let provider = SwiftProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "H", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "run", &[3, 8]);
    uid_chain_support::assert_own_members(
        &g,
        ecp_core::graph::RelType::HasMethod,
        "run",
        &[(3, 2), (8, 7)],
    );
}

const LOCAL_TWINS: &str = "func a() {\n    func l() {}\n}\nfunc b() {\n    func l() {}\n}\n";

/// Stands for: L (C1-identity.md "Collision shapes per language"). A local
/// `func l` in each of two functions is two named nodes with two uids.
#[test]
fn test_build_swift_local_func_in_two_functions_two_nodes() {
    uid_chain_support::assert_twins(
        &SwiftProvider::new().unwrap(),
        PATH,
        LOCAL_TWINS,
        "l",
        &[2, 5],
    );
}

const EXTENSION_BESIDE_TYPE: &str =
    "class K {\n    func m(_ x: Int) {}\n}\nextension K {\n    func m(_ s: String) {}\n}\n";

/// Stands for: a method in `extension K` joins the set of the same-named
/// method in `K` (identity `joined_containers`): two named nodes, the
/// in-type one first, and its `overload_set` reference reaches the
/// extension one, so a call by name has one candidate.
#[test]
fn test_build_swift_extension_method_joins_in_type_method_set() {
    let g = uid_chain_support::assert_twins(
        &SwiftProvider::new().unwrap(),
        PATH,
        EXTENSION_BESIDE_TYPE,
        "m",
        &[2, 5],
    );
    let ext = uid_chain_support::node_at(&g, "m", 5);
    assert_eq!(
        uid_chain_support::source_lines(&g, RelType::References, ext),
        vec![2]
    );
}

const PROTOCOL_DEFAULT_FIELD: &str =
    "protocol P {\n    var af: Int { get }\n}\nextension P {\n    var af: Int { 1 }\n}\n";

/// Stands for: a field in `extension P` joins the same-named requirement of
/// protocol `P` (identity `joined_containers`) and merges into it, so a
/// read of `x.af` has one candidate.
#[test]
fn test_build_swift_extension_field_merges_into_protocol_requirement() {
    let g = uid_chain_support::build(&SwiftProvider::new().unwrap(), PATH, PROTOCOL_DEFAULT_FIELD);
    assert_eq!(
        uid_chain_support::named(&g, "af")
            .iter()
            .map(|&(l, _)| l)
            .collect::<Vec<_>>(),
        vec![2]
    );
}
