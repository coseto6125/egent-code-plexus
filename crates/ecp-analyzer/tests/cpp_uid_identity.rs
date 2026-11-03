//! Declaration uids that keep same-named declarations distinct, in C++
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `#ifdef` twins of `int f()` |
//! | FO | `void g(int)` and `void g(double)` at the top level |
//! | M | none: C++ declares no type in parts |
//! | enum | `enum E { X }` in namespaces `n` and `m` |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

use crate::uid_chain_support;

const PATH: &str = "m.cpp";

fn parse(src: &str) -> LocalGraph {
    CppProvider::new()
        .expect("CppProvider::new")
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

const IFDEF_TWINS: &str = r#"int a() { return 1; }
int b() { return 2; }
#ifdef X
int f() { return a(); }
#else
int f() { return b(); }
#endif
"#;

const OVERLOADS: &str = r#"int a() { return 1; }
int b() { return 2; }
void g(int x) { a(); }
void g(double x) { b(); }
"#;

const NAMESPACE_ENUMS: &str = r#"namespace n {
enum E { X };
}
namespace m {
enum E { X };
}
"#;

#[test]
fn test_build_cpp_ifdef_twins_keep_two_nodes_with_own_calls() {
    let g = build(IFDEF_TWINS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

#[test]
fn test_build_cpp_top_level_overloads_keep_two_nodes_with_own_calls() {
    let g = build(OVERLOADS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "g",
        RelType::Defines,
        Some(("a", "b")),
    );
}

/// Stands for: enum containment binds by span and row (design_uid §3
/// row 22). `n::E` and `m::E` each define the `X` of their own body.
#[test]
fn test_enum_variant_defines_two_mods_same_enum_binds_by_span() {
    assert_enum_rows(NAMESPACE_ENUMS, "X", &[(2, 2), (5, 5)]);
}

const NESTED_TWINS: &str = "class O1 {\n    class H {\n        void run() {}\n    };\n};\nclass O2 {\n    class H {\n        void run() {}\n    };\n};\n";

/// Stands for: N1 and N2 (design_uid §5, phase 2). `O1.H` and `O2.H` are
/// two named nodes with two uids on the full build and the overlay, and each
/// `run` is a member of its own `H` only, with no `overload_set` reference
/// between the two `run`.
#[test]
fn test_build_cpp_twin_nested_types_two_nodes_own_members() {
    let provider = CppProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "H", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NESTED_TWINS, "run", &[3, 8]);
    uid_chain_support::assert_own_members(
        &g,
        ecp_core::graph::RelType::HasMethod,
        "run",
        &[(3, 2), (8, 7)],
    );
}

const NAMESPACE_TWINS: &str = "namespace n {\nnamespace i {\nvoid f() {}\n}\n}\nnamespace m {\nnamespace i {\nvoid f() {}\n}\n}\n";

/// Stands for: NS (design_uid §5, phase 2). `n::i::f` and `m::i::f` chain
/// on two different `i` anchors: two named `i`, two named `f`.
#[test]
fn test_build_cpp_twin_namespace_paths_two_nodes() {
    let provider = CppProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, NAMESPACE_TWINS, "i", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NAMESPACE_TWINS, "f", &[3, 8]);
}

const LOCAL_TWINS: &str = "void a() {\n    struct L {\n        int x;\n    };\n}\nvoid b() {\n    struct L {\n        int x;\n    };\n}\n";

/// Stands for: L (design_uid §5, phase 2). A local `struct L` in each of
/// two functions is two named nodes, and so is the field of each.
#[test]
fn test_build_cpp_local_struct_in_two_functions_two_nodes() {
    let provider = CppProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_TWINS, "L", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_TWINS, "x", &[3, 8]);
}

const OUT_OF_LINE: &str = "class K {\n    void m();\n};\nvoid K::m() {}\n";

/// Stands for: an out-of-line `K::m` joins the set of the in-class `m`
/// (identity `joined_containers`): two named nodes, the declaration first,
/// and its `overload_set` reference reaches the definition, so `impact`
/// from the body reaches the callers bound to the declaration.
#[test]
fn test_build_cpp_out_of_line_definition_joins_in_class_declaration_set() {
    let provider = CppProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, OUT_OF_LINE, "m", &[2, 4]);
    let def = uid_chain_support::node_at(&g, "m", 4);
    assert_eq!(
        uid_chain_support::source_lines(&g, RelType::References, def),
        vec![2]
    );
}
