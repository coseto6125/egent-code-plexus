//! Declaration uids that keep same-named declarations distinct, in Dart
//! (design_uid §5 rows R / FO / M, Phase 1 acceptance). A redefinition of a
//! kind that keeps its own node is a second named node with its own uid, its
//! own parent edge and its own calls; membership and containment edges bind
//! the row that declares them; a merged repeat gives each edge once.
//!
//! | Shape | Fixture |
//! |---|---|
//! | R | none: Dart has no conditional compilation, and a repeat is a compile error |
//! | FO | none: Dart has no overloads |
//! | M | none: Dart declares no type in parts |
//! | merge | a getter and a setter of `v`: one Property |
//!
//! Every assertion reads the built graph a consumer reads.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

mod uid_chain_support;

const PATH: &str = "m.dart";

fn parse(src: &str) -> LocalGraph {
    DartProvider::new()
        .expect("DartProvider::new")
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

fn uid_collisions(g: &ZeroCopyGraph) -> usize {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .filter(|b| b.kind.resolve(pool) == "uid-collision")
        .count()
}

const GETTER_SETTER: &str = r#"class K {
  int _v = 0;
  int get v => _v;
  set v(int x) {
    _v = x;
  }
}
"#;

/// Stands for: the merge class (design_uid §2.3) keeps one named node, and
/// a merged row gives its edge once (§3 row 5): one `HasProperty K -> v`.
#[test]
fn test_build_dart_getter_and_setter_merge_into_one_property() {
    let g = build(GETTER_SETTER);
    let v = named(&g, NodeKind::Property, "v");
    assert_eq!(v.len(), 1, "one named `v`");
    assert_eq!(
        sources(&g, RelType::HasProperty, v[0]),
        named(&g, NodeKind::Class, "K")
    );
    assert_eq!(uid_collisions(&g), 0);
}

const LOCAL_TWINS: &str = "void a() {\n  void l() {}\n}\nvoid b() {\n  void l() {}\n}\n";

/// Stands for: L (design_uid §5, phase 2). A local function `l` in each of
/// two functions is two named nodes with two uids.
#[test]
fn test_build_dart_local_function_in_two_functions_two_nodes() {
    uid_chain_support::assert_twins(
        &DartProvider::new().unwrap(),
        PATH,
        LOCAL_TWINS,
        "l",
        &[2, 5],
    );
}
