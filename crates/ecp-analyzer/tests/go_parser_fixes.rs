//! Go parser fixes: a top-level `const` node spans its own spec, not the file.

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{LocalGraph, RawNode};
use ecp_core::graph::NodeKind;
use ecp_core::uid::xxh3_64_bytes;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    GoProvider::new()
        .expect("GoProvider::new")
        .parse_file(Path::new("a.go"), src.as_bytes())
        .expect("parse_file")
}

fn konst<'g>(g: &'g LocalGraph, name: &str) -> &'g RawNode {
    g.nodes
        .iter()
        .find(|n| n.name == name && n.kind == NodeKind::Const)
        .unwrap_or_else(|| panic!("no Const {name}"))
}

// FU-2026-10-30-8521e844bbe9
#[test]
fn test_go_const_single_span_covers_only_its_own_line() {
    let src = "package p\n\nconst A = 1\n\nfunc f() {}\n";
    let g = parse(src);
    let n = konst(&g, "A");
    assert_eq!((n.span.0, n.span.2), (2, 2), "span {:?}", n.span);
    assert_ne!(n.content_hash, xxh3_64_bytes(src.as_bytes()));
}

// FU-2026-10-30-8521e844bbe9
#[test]
fn test_go_const_block_members_span_their_own_rows_with_distinct_hashes() {
    let src = "package p\n\nconst (\n\tA = 1\n\tB = 2\n)\n";
    let g = parse(src);
    let (a, b) = (konst(&g, "A"), konst(&g, "B"));
    assert_eq!((a.span.0, a.span.2), (3, 3), "span {:?}", a.span);
    assert_eq!((b.span.0, b.span.2), (4, 4), "span {:?}", b.span);
    assert_ne!(a.content_hash, b.content_hash);
    assert_ne!(a.content_hash, xxh3_64_bytes(src.as_bytes()));
}

// FU-2026-10-30-8521e844bbe9
#[test]
fn test_go_const_two_declarations_do_not_share_span_or_hash() {
    let g = parse("package p\n\nconst A = 1\nconst B = 2\n");
    let (a, b) = (konst(&g, "A"), konst(&g, "B"));
    assert_ne!(a.span, b.span);
    assert_ne!(a.content_hash, b.content_hash);
}

// FU-2026-10-30-8521e844bbe9
#[test]
fn test_go_const_one_spec_with_two_names_emits_both_on_that_spec() {
    let g = parse("package p\n\nconst X, Y = 1, 2\n");
    let (x, y) = (konst(&g, "X"), konst(&g, "Y"));
    assert_eq!((x.span.0, x.span.2), (2, 2), "span {:?}", x.span);
    assert_eq!(x.span, y.span);
}

// FU-2026-10-30-8521e844bbe9 guard: function-local const stays dropped.
#[test]
fn test_go_function_local_const_stays_dropped() {
    let g = parse("package p\n\nfunc f() {\n\tconst L = 1\n}\n");
    assert!(g.nodes.iter().all(|n| n.kind != NodeKind::Const));
}
