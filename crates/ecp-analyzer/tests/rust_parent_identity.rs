//! Slice R-PI: a Rust impl member's anchor carries the trait of its `impl`
//! block (`Discriminator::ImplTrait`, C1), so a parent keeps its identity
//! before a scoped child hashes it, and pairing reads that identity, never an
//! ordinal (`docs/specs/contracts/C1-identity.md`, "File assignment" and
//! "Overlay pairing").
//!
//! Every fixture runs through the Rust parser, so the spans and owners are
//! the ones the index and the overlay read.

use ecp_analyzer::rust::parser::RustProvider;
use ecp_core::analyzer::lexical::{file_identities, raw_decl, stored_uid};
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::contracts::identity::{
    assign_file, lexical_parents, pair_by_anchor, DeclIdentity, FileDecl, UidSlot, Uniqueness,
};
use ecp_core::graph::NodeKind;
use std::collections::HashMap;
use std::path::Path;

const PATH: &str = "src/a.rs";

fn parse(source: &str) -> LocalGraph {
    RustProvider::new()
        .expect("RustProvider::new")
        .parse_file(Path::new(PATH), source.as_bytes())
        .expect("parse_file")
}

fn identities(local: &LocalGraph) -> Vec<DeclIdentity> {
    file_identities(PATH, &local.nodes, raw_decl)
}

/// The offsets of the nodes named `name`, keyed by the owner of the `Impl`
/// node whose rows hold each one: the trait text.
fn by_trait(local: &LocalGraph, name: &str) -> HashMap<String, usize> {
    let impls: Vec<_> = local
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Impl)
        .collect();
    local
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name == name)
        .map(|(i, n)| {
            let block = impls
                .iter()
                .find(|b| b.span.0 <= n.span.0 && n.span.2 <= b.span.2)
                .unwrap_or_else(|| panic!("`{name}` at {:?} is in no impl", n.span));
            (block.owner_class.clone().unwrap_or_default(), i)
        })
        .collect()
}

fn fmt_impl(trait_name: &str) -> String {
    format!(
        "impl {trait_name} for A {{\n    fn fmt(&self) {{\n        fn pad() {{}}\n        pad();\n    }}\n}}\n"
    )
}

fn swapped(first: &str, second: &str) -> LocalGraph {
    parse(&format!(
        "struct A;\n{}{}",
        fmt_impl(first),
        fmt_impl(second)
    ))
}

/// The stored uid of `fmt` itself stays positional (C1 Open point 1 (a)):
/// the discriminator enters the anchor only. What an edit keeps is the
/// anchor, which pairing and every scoped child read
/// (`test_assign_file_swapped_impls_keep_child_uids` in `contracts::identity`
/// covers the child that hashes it).
#[test]
fn test_assign_file_rust_swapped_impls_fmt_keeps_anchor() {
    let (before, after) = (swapped("Display", "Debug"), swapped("Debug", "Display"));
    let (before_ids, after_ids) = (identities(&before), identities(&after));
    let (fmt_before, fmt_after) = (by_trait(&before, "fmt"), by_trait(&after, "fmt"));
    let (pad_before, pad_after) = (by_trait(&before, "pad"), by_trait(&after, "pad"));
    assert_eq!(
        fmt_before.len(),
        2,
        "the fixture's two `fmt`: {fmt_before:?}"
    );
    for t in ["Display", "Debug"] {
        let (b, a) = (&before_ids[fmt_before[t]], &after_ids[fmt_after[t]]);
        assert_eq!(b.anchor, a.anchor, "the `fmt` of {t} keeps its anchor");
        assert_eq!(b.uniqueness, Uniqueness::Unique, "{t}: before");
        assert_eq!(a.uniqueness, Uniqueness::Unique, "{t}: after");
        // Today the nested `pad` carries the lexical path, trait included, as
        // its owner; R6 moves it to a scoped uid over the `fmt` anchor.
        assert_eq!(
            stored_uid(before_ids[pad_before[t]].slot),
            stored_uid(after_ids[pad_after[t]].slot),
            "the `pad` under the `fmt` of {t} keeps its uid"
        );
    }
    assert_ne!(
        before_ids[fmt_before["Display"]].anchor,
        before_ids[fmt_before["Debug"]].anchor
    );
    // The stored uid follows source order: the swap moves the first slot.
    assert!(matches!(
        before_ids[fmt_before["Display"]].slot,
        UidSlot::First(_)
    ));
    assert!(matches!(
        after_ids[fmt_after["Display"]].slot,
        UidSlot::Later { ordinal: 1, .. }
    ));
}

/// Two inherent `impl A` blocks each declare `f`: equal anchors, so the
/// ordinal is the only thing that tells them apart, and it pairs nothing.
/// The trait-impl `f` has its own anchor and pairs with its base twin.
#[test]
fn test_pair_by_anchor_ordinal_only_pairs_nothing() {
    let base = parse(
        "struct A;\nimpl A {\n    fn f(&self) {}\n}\nimpl A {\n    fn f(&self) {}\n}\nimpl Display for A {\n    fn f(&self) {}\n}\n",
    );
    let dirty = parse(
        "struct A;\nimpl A {\n    fn f(&self) {}\n}\nimpl A {\n    fn f(&self) { g(); }\n}\nimpl Display for A {\n    fn f(&self) { g(); }\n}\nfn g() {}\n",
    );
    let anchors = |local: &LocalGraph| -> Vec<u64> {
        identities(local).into_iter().map(|id| id.anchor).collect()
    };
    let pairs = pair_by_anchor(&anchors(&base), &anchors(&dirty));
    let methods = |local: &LocalGraph| -> Vec<usize> {
        (0..local.nodes.len())
            .filter(|&i| local.nodes[i].kind == NodeKind::Method && local.nodes[i].name == "f")
            .collect()
    };
    let (base_f, dirty_f) = (methods(&base), methods(&dirty));
    assert_eq!(dirty_f.len(), 3, "the fixture's three `f`");
    let dirty_traits = by_trait(&dirty, "f");
    let base_traits = by_trait(&base, "f");
    assert_eq!(
        pairs[dirty_traits["Display"]],
        Some(base_traits["Display"] as u32),
        "the trait-impl `f` pairs with its own base node"
    );
    let inherent: Vec<usize> = dirty_f
        .iter()
        .copied()
        .filter(|&i| i != dirty_traits["Display"])
        .collect();
    assert_eq!(inherent.len(), 2);
    for i in inherent {
        assert_eq!(pairs[i], None, "an inherent `f` at {i} shares its anchor");
    }
    assert_eq!(base_f.len(), 3);
}

/// One function holds two blocks, and each block declares `h`.
#[test]
fn test_assign_file_sibling_blocks_same_name_marked_ambiguous() {
    let local = parse(
        "fn outer() {\n    {\n        fn h() {}\n    }\n    {\n        fn h() {}\n    }\n}\n",
    );
    let hs: Vec<usize> = (0..local.nodes.len())
        .filter(|&i| local.nodes[i].name == "h")
        .collect();
    assert_eq!(hs.len(), 2, "the parser emits both `h`");
    let (h1, h2) = (hs[0], hs[1]);

    // Today: both `h` carry the owner `outer`, so they share an anchor and
    // are marked ambiguous. The second is a `Duplicate`, which the index
    // tombstones; R6 flips that once Rust scopes nested functions.
    let today = identities(&local);
    let shared = |position| Uniqueness::Shared { position, count: 2 };
    assert_eq!(today[h1].uniqueness, shared(0));
    assert_eq!(today[h2].uniqueness, shared(1));
    assert!(matches!(today[h2].slot, UidSlot::Duplicate { .. }));

    // The R6 input shape: the parser's spans give each `h` the parent
    // `outer`, and a scoped row has no owner (C1 invariant I1).
    let spans: Vec<(NodeKind, (u32, u32, u32, u32))> =
        local.nodes.iter().map(|n| (n.kind, n.span)).collect();
    let parents = lexical_parents(&spans, &[NodeKind::Function]);
    let outer = (0..local.nodes.len())
        .find(|&i| local.nodes[i].name == "outer")
        .expect("outer");
    assert_eq!(
        (parents[h1], parents[h2]),
        (Some(outer as u32), Some(outer as u32))
    );
    let decls: Vec<FileDecl<'_>> = local
        .nodes
        .iter()
        .zip(&parents)
        .map(|(n, &parent)| FileDecl {
            kind: n.kind,
            owner: n.owner_class.as_deref().filter(|_| parent.is_none()),
            name: &n.name,
            parent,
            discriminator: None,
            arity: None,
        })
        .collect();
    let scoped = assign_file(PATH, &decls).expect("scoped rows are valid");
    assert_eq!(scoped[h1].uniqueness, shared(0));
    assert_eq!(scoped[h2].uniqueness, shared(1));
    assert!(
        matches!(scoped[h2].slot, UidSlot::Later { ordinal: 1, .. }),
        "the second `h` stays a node: {:?}",
        scoped[h2].slot
    );
    assert_ne!(scoped[h1].slot.uid(), scoped[h2].slot.uid());
    let anchors: Vec<u64> = scoped.iter().map(|id| id.anchor).collect();
    let pairs = pair_by_anchor(&anchors, &anchors);
    assert_eq!(
        (pairs[h1], pairs[h2]),
        (None, None),
        "an ambiguous `h` pairs nothing"
    );
    assert_eq!(pairs[outer], Some(outer as u32), "a unique `outer` pairs");
}
