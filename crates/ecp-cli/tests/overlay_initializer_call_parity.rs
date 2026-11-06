//! A call added to a Java static block or a C# property getter has a holder
//! (the class `T`, the property `X`). The session overlay yields the same
//! `Calls` rows as a full reindex of the edited tree, and the rows are not
//! empty, so the holder is in the compared set.

use crate::parity;

use parity::{calls_after_edit, named, parse};

#[test]
fn test_overlay_java_static_block_call_matches_reindex() {
    let before = "class T { static void target() {} static { } }";
    let after = "class T { static void target() {} static { target(); } }";
    let parity = calls_after_edit(
        &parse,
        &[("T.java", before)],
        &[("T.java", after)],
        &named("T"),
    );
    assert!(
        parity.reindex.iter().any(|edge| edge.callee == "target"),
        "the reindex must hold T -> target, got {:#?}",
        parity.reindex
    );
    assert_eq!(parity.reindex, parity.overlay);
}

#[test]
fn test_overlay_csharp_property_getter_call_matches_reindex() {
    let before = "class T { static int Target() { return 1; } int X { get { return 1; } } }";
    let after = "class T { static int Target() { return 1; } int X { get { return Target(); } } }";
    let parity = calls_after_edit(&parse, &[("T.cs", before)], &[("T.cs", after)], &named("X"));
    assert!(
        parity.reindex.iter().any(|edge| edge.callee == "Target"),
        "the reindex must hold X -> Target, got {:#?}",
        parity.reindex
    );
    assert_eq!(parity.reindex, parity.overlay);
}

#[test]
fn test_overlay_java_nested_static_block_call_binds_inner_target_like_reindex() {
    let before =
        "class O { static void target() {} static class N { static void target() {} static { } } }";
    let after = "class O { static void target() {} static class N { static void target() {} static { target(); } } }";
    let parity = calls_after_edit(
        &parse,
        &[("O.java", before)],
        &[("O.java", after)],
        &named("N"),
    );
    assert!(
        parity
            .reindex
            .iter()
            .any(|edge| edge.callee == "target" && edge.callee_line == 1),
        "the reindex must hold N -> target, got {:#?}",
        parity.reindex
    );
    assert_eq!(parity.reindex, parity.overlay);
}
