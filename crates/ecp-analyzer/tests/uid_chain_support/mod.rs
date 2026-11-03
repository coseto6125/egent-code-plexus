//! Shared helpers for the container-chain cases (C1-identity.md "Collision
//! shapes per language") of the `<lang>_uid_identity` tests: the shapes N1 (a
//! member of one of two same-named nested types), N2 (the two nested types), NS
//! (one module path under two modules) and L (a local declaration in two
//! callables).
#![allow(dead_code)]

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, OVERLOAD_SET_REASON};
use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType, ZeroCopyGraph};
use ecp_core::session::{OverlayFileInput, OverlayView};
use std::path::Path;

pub fn parse(provider: &dyn LanguageProvider, path: &str, src: &str) -> LocalGraph {
    provider
        .parse_file(Path::new(path), src.as_bytes())
        .expect("parse_file")
}

pub fn build(provider: &dyn LanguageProvider, path: &str, src: &str) -> ZeroCopyGraph {
    let mut builder = GraphBuilder::new();
    builder.add_graph(parse(provider, path, src));
    builder.build()
}

/// `(1-based start line, node index)` of every node named `name`, sorted. A
/// `Process` node takes its entry function's name, so it never counts.
pub fn named(g: &ZeroCopyGraph, name: &str) -> Vec<(u32, u32)> {
    let pool = g.string_pool.as_slice();
    let mut out: Vec<(u32, u32)> = (0..g.nodes.len() as u32)
        .filter(|&i| {
            let n = &g.nodes[i as usize];
            n.kind != NodeKind::Process && n.name.resolve(pool) == name
        })
        .map(|i| (g.nodes[i as usize].span.0 + 1, i))
        .collect();
    out.sort_unstable();
    out
}

/// The node named `name` that starts on 1-based `line`.
pub fn node_at(g: &ZeroCopyGraph, name: &str, line: u32) -> u32 {
    named(g, name)
        .into_iter()
        .find(|&(l, _)| l == line)
        .map(|(_, i)| i)
        .unwrap_or_else(|| panic!("no `{name}` at line {line}: {:?}", named(g, name)))
}

pub fn uid_collisions(g: &ZeroCopyGraph) -> usize {
    let pool = g.string_pool.as_slice();
    g.blind_spots
        .iter()
        .filter(|b| b.kind.resolve(pool) == "uid-collision")
        .count()
}

/// The twin shape: `src` builds one named node `name` per 1-based start
/// line of `lines` and no other, with distinct uids and no `uid-collision`
/// BlindSpot. The overlay view of the same file over that build gives the
/// same uids, so the dirty path agrees with the full build. Returns the
/// full build.
pub fn assert_twins(
    provider: &dyn LanguageProvider,
    path: &str,
    src: &str,
    name: &str,
    lines: &[u32],
) -> ZeroCopyGraph {
    let g = build(provider, path, src);
    let hits = named(&g, name);
    assert_eq!(
        hits.iter().map(|&(l, _)| l).collect::<Vec<_>>(),
        lines,
        "{path}: one named `{name}` per declaration"
    );
    let mut uids: Vec<u64> = hits.iter().map(|&(_, i)| g.nodes[i as usize].uid).collect();
    uids.sort_unstable();
    let distinct = uids.windows(2).all(|w| w[0] != w[1]);
    assert!(distinct, "{path}: one uid per `{name}`: {uids:x?}");
    assert_eq!(uid_collisions(&g), 0, "{path}: no uid-collision");

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).expect("archive graph");
    let base =
        rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).expect("access graph");
    let view = OverlayView::build(base, &[OverlayFileInput::from(parse(provider, path, src))])
        .expect("a view");
    let mut virt: Vec<u64> = view
        .virtual_nodes()
        .iter()
        .filter(|n| n.kind != NodeKind::Process && n.name == name)
        .map(|n| n.uid)
        .collect();
    virt.sort_unstable();
    assert_eq!(virt, uids, "{path}: overlay view vs full build, `{name}`");
    g
}

/// The 1-based start lines of the sources of every `rel` edge into `target`.
pub fn source_lines(g: &ZeroCopyGraph, rel: RelType, target: u32) -> Vec<u32> {
    let mut out: Vec<u32> = g
        .edges
        .iter()
        .filter(|e| e.rel_type == rel && e.target == target)
        .map(|e| g.nodes[e.source as usize].span.0 + 1)
        .collect();
    out.sort_unstable();
    out
}

/// The N1 shape: each `(member line, owner line)` pair names a member
/// `member` whose one `rel` edge comes from the owner on that line, and no
/// `overload_set` reference starts or ends at any `member`.
pub fn assert_own_members(g: &ZeroCopyGraph, rel: RelType, member: &str, pairs: &[(u32, u32)]) {
    for &(line, owner) in pairs {
        let target = node_at(g, member, line);
        assert_eq!(
            source_lines(g, rel, target),
            vec![owner],
            "`{member}` at line {line}: one {rel:?}, from line {owner}"
        );
    }
    assert_eq!(
        overload_set_refs(g, member),
        0,
        "no overload_set on `{member}`"
    );
}

/// The `overload_set` references that start or end at a node named `name`.
pub fn overload_set_refs(g: &ZeroCopyGraph, name: &str) -> usize {
    let pool = g.string_pool.as_slice();
    let named_end = |idx: u32| g.nodes[idx as usize].name.resolve(pool) == name;
    g.edges
        .iter()
        .filter(|e| e.reason.resolve(pool) == OVERLOAD_SET_REASON)
        .filter(|e| named_end(e.source) || named_end(e.target))
        .count()
}

/// The `Calls` and `References` edges into `target`: what `ecp impact`
/// lists as its callers.
pub fn callers(g: &ZeroCopyGraph, target: u32) -> usize {
    g.edges
        .iter()
        .filter(|e| e.target == target)
        .filter(|e| matches!(e.rel_type, RelType::Calls | RelType::References))
        .count()
}
