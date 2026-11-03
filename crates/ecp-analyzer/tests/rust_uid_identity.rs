//! Declaration uids that keep same-named declarations distinct, in Rust
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | `#[cfg]` twins of `fn f` and of `enum E { X }` |
//! | FO | none: Rust has no overloads |
//! | M | none: Rust declares no type in parts |
//! | enum | `enum E { X }` in `mod a` and in `mod b` |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{MergedGraph, OverlayFileInput, OverlayView};
use std::path::Path;

use crate::uid_chain_support;

const PATH: &str = "m.rs";

fn parse(src: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
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

const CFG_FN_TWINS: &str = r#"fn a() {}
fn b() {}

#[cfg(unix)]
fn f() {
    a()
}

#[cfg(not(unix))]
fn f() {
    b()
}
"#;

const CFG_ENUM_TWINS: &str = r#"#[cfg(unix)]
enum E {
    X,
}

#[cfg(not(unix))]
enum E {
    X,
}
"#;

const TWO_IMPLS: &str = r#"struct S;

impl S {
    fn a(&self) {}
}

impl S {
    fn b(&self) {}
}
"#;

const MOD_ENUMS: &str = r#"mod a {
    pub enum E {
        X,
    }
}

mod b {
    pub enum E {
        X,
    }
}
"#;

#[test]
fn test_build_rust_cfg_fn_twins_keep_two_nodes_with_own_calls() {
    let g = build(CFG_FN_TWINS);
    assert_two_kept(
        &g,
        NodeKind::Function,
        "f",
        RelType::Defines,
        Some(("a", "b")),
    );
}

/// Stands for: enum containment binds by span and row (design_uid §3
/// row 22). Each `#[cfg]` twin keeps its node and defines the `X` of its
/// own body.
#[test]
fn test_enum_variant_defines_two_mods_same_enum_binds_by_span() {
    let g = build(CFG_ENUM_TWINS);
    assert_two_kept(&g, NodeKind::Enum, "E", RelType::Defines, None);
    assert_enum_rows(CFG_ENUM_TWINS, "X", &[(2, 3), (7, 8)]);
}

/// Stands for: enum containment binds by span and row. `a::E` and `b::E`
/// differ by owner, their variants do not; each enum defines the `X` of its
/// own body (design_uid Phase 2 acceptance, which the span binding already
/// meets).
#[test]
fn test_enum_variant_defines_two_mods_each_enum_defines_own_variant() {
    assert_enum_rows(MOD_ENUMS, "X", &[(2, 3), (8, 9)]);
}

/// Stands for: `impl S` twice with a different method in each (design_uid §5
/// row R, spec #5) is two `Impl` nodes with distinct uids and no
/// `uid-collision`. The build gives each method its `HasMethod` from the
/// owner `Struct S`, once each: no `Impl` node is a member's source, so the
/// two blocks do not change who owns `a` and `b`.
#[test]
fn test_build_rust_impl_twice_keeps_two_impl_nodes_methods_owned_by_struct() {
    let g = build(TWO_IMPLS);
    let impls = named(&g, NodeKind::Impl, "S");
    assert_eq!(impls.len(), 2, "two named `impl S` nodes");
    assert_ne!(
        g.nodes[impls[0] as usize].uid,
        g.nodes[impls[1] as usize].uid
    );
    assert_eq!(uid_collisions(&g), 0);
    let owner = named(&g, NodeKind::Struct, "S");
    assert_eq!(owner.len(), 1, "one `struct S`");
    let (a, b) = (callable(&g, "a"), callable(&g, "b"));
    let mut expected = vec![(owner[0], a), (owner[0], b)];
    expected.sort_unstable();
    assert_eq!(edges(&g, RelType::HasMethod), expected);
}

/// Stands for: the container chain (design_uid phase 2). `a::E` and `b::E`
/// key on two module anchors, so they are two named nodes with two uids,
/// and so are their two `X`, on the full build and the overlay.
#[test]
fn test_build_rust_two_mods_same_enum_two_enums_two_variants() {
    let provider = RustProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, MOD_ENUMS, "E", &[2, 8]);
    uid_chain_support::assert_twins(&provider, PATH, MOD_ENUMS, "X", &[3, 9]);
}

const NS_TWINS: &str = "mod o1 {\n    mod inner {\n        pub fn f() {}\n    }\n}\nmod o2 {\n    mod inner {\n        pub fn f() {}\n    }\n}\n";

/// Stands for: NS (design_uid §5, phase 2). `o1::inner::f` and
/// `o2::inner::f` chain on two different `inner` anchors: two named
/// `inner`, two named `f`.
#[test]
fn test_build_rust_twin_module_paths_two_nodes() {
    let provider = RustProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, NS_TWINS, "inner", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, NS_TWINS, "f", &[3, 8]);
}

const LOCAL_CONST_TWINS: &str = "fn a() {\n    fn helper() {\n        const X: u8 = 1;\n    }\n}\nfn b() {\n    fn helper() {\n        const X: u8 = 2;\n    }\n}\n";

/// Stands for: a Const keeps its callable container (design_uid §2.1), so
/// a local `const X` in each of two nested `helper`s is two named `X` with
/// two uids, not one merged entity.
#[test]
fn test_assign_file_rust_local_const_in_two_helpers_two_nodes() {
    let provider = RustProvider::new().unwrap();
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_CONST_TWINS, "helper", &[2, 7]);
    uid_chain_support::assert_twins(&provider, PATH, LOCAL_CONST_TWINS, "X", &[3, 8]);
}

const TWO_MOD_IMPLS: &str = "mod a {\n    pub struct S;\n    impl S {\n        fn m(&self) {}\n    }\n}\nmod b {\n    pub struct S;\n    impl S {\n        fn m(&self) {}\n    }\n}\n";

/// Stands for: the owner pass binds the type row beside the `impl` block
/// (design_uid phase 2, rule 1): each `m` is a member of the `S` of its own
/// module, once.
#[test]
fn test_class_membership_rust_two_mods_same_impl_binds_own_method() {
    let provider = RustProvider::new().unwrap();
    let g = uid_chain_support::assert_twins(&provider, PATH, TWO_MOD_IMPLS, "m", &[4, 10]);
    uid_chain_support::assert_own_members(&g, RelType::HasMethod, "m", &[(4, 2), (10, 8)]);
}

const IMPORTED_RECEIVER: &str = "mod b;\nmod a {\n    pub struct S;\n}\nmod c {\n    use super::b::S;\n    impl S {\n        fn m(&self) {}\n    }\n}\n";

/// Stands for: an `impl` of a type no row beside the block declares (here
/// imported from `b`) binds no same-named `a::S` of the file (design_uid
/// phase 2, rule 2); the `impl` block on line 7 owns `m`, so `m` keeps a
/// parent.
#[test]
fn test_class_membership_rust_impl_imported_receiver_binds_impl_block() {
    let provider = RustProvider::new().unwrap();
    let g = uid_chain_support::build(&provider, "src/lib.rs", IMPORTED_RECEIVER);
    let m = uid_chain_support::node_at(&g, "m", 8);
    assert_eq!(
        uid_chain_support::source_lines(&g, RelType::HasMethod, m),
        vec![7]
    );
}

const IMPL_ONLY: &str = "use crate::model::S;\nimpl S {\n    fn m(&self) {}\n}\n";

/// Stands for: a file of `impl` blocks alone, for a type of another file,
/// still gives each member its block as owner (owner pass, rule 2).
#[test]
fn test_class_membership_rust_impl_only_file_binds_impl_block() {
    let provider = RustProvider::new().unwrap();
    let g = uid_chain_support::build(&provider, "src/ext.rs", IMPL_ONLY);
    let m = uid_chain_support::node_at(&g, "m", 3);
    assert_eq!(
        uid_chain_support::source_lines(&g, RelType::HasMethod, m),
        vec![2]
    );
}
