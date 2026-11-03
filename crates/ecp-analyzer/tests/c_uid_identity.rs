//! Declaration uids that keep same-named declarations distinct, in C
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `#ifdef` twins of `int f` and of `struct S` |
//! | FO | none: C has no overloads |
//! | M | none: C declares no type in parts |
//! | merge | a field and a variable in each branch of `#if` / `#elif` / `#else` |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

mod uid_chain_support;

const PATH: &str = "m.c";

fn parse(src: &str) -> LocalGraph {
    CProvider::new()
        .expect("CProvider::new")
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

const IFDEF_FN_TWINS: &str = r#"int a(void) { return 1; }
int b(void) { return 2; }

#ifdef X
int f(void) { return a(); }
#else
int f(void) { return b(); }
#endif
"#;

const IFDEF_STRUCT_TWINS: &str = r#"#ifdef X
struct S { int p; };
#else
struct S { int q; };
#endif
"#;

const IF_ELIF_ELSE: &str = r#"struct T {
#if A
    int v;
#elif B
    int v;
#else
    int v;
#endif
};

#if A
int g;
#elif B
int g;
#else
int g;
#endif
"#;

#[test]
fn test_build_c_ifdef_fn_twins_keep_two_nodes_with_own_calls() {
    let g = build(IFDEF_FN_TWINS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

/// Stands for: a redefined struct keeps its node and owns the fields of its
/// own body (design_uid §2.3, §3 row 5).
#[test]
fn test_build_c_ifdef_struct_twins_each_own_their_own_field() {
    let g = build(IFDEF_STRUCT_TWINS);
    let (first, second) = assert_two_kept(&g, NodeKind::Struct, "S", RelType::Defines, None);
    let (p, q) = (
        named(&g, NodeKind::Property, "p"),
        named(&g, NodeKind::Property, "q"),
    );
    assert_eq!(
        edges(&g, RelType::HasProperty),
        sorted(vec![(first, p[0]), (second, q[0])])
    );
}

/// Stands for: the merge class (design_uid §2.3) keeps one named node, and
/// a merged row gives its edge once (§3 rows 4, 5): one `HasProperty`
/// `T -> v`, one `File -> g`.
#[test]
fn test_build_c_if_elif_else_field_and_variable_merge_into_one_node() {
    let g = build(IF_ELIF_ELSE);
    let v = named(&g, NodeKind::Property, "v");
    let t = named(&g, NodeKind::Struct, "T");
    assert_eq!(v.len(), 1, "one named `v`");
    assert_eq!(edges(&g, RelType::HasProperty), [(t[0], v[0])]);
    let var = named(&g, NodeKind::Variable, "g");
    assert_eq!(var.len(), 1, "one named `g`");
    assert_eq!(sources(&g, RelType::Defines, var[0]).len(), 1);
    assert_eq!(uid_collisions(&g), 0);
}

const LOCAL_TWINS: &str = "void a(void) {\n    struct L {\n        int x;\n    };\n}\nvoid b(void) {\n    struct L {\n        int x;\n    };\n}\n";

/// Stands for: L (design_uid §5, phase 2). A local `struct L` in each of
/// two functions is two named nodes, and so is the field of each: a field
/// keys on its struct's anchor.
#[test]
fn test_build_c_local_struct_in_two_functions_two_nodes() {
    let provider = CProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_TWINS, "L", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_TWINS, "x", &[3, 8]);
}
