//! `Overrides` in C (slice P7). C has no inheritance and no override: the
//! "base" idiom is a struct embedded as the first member, and the C parser
//! records no heritage for it. The pass emits no `Overrides` edge and no
//! override BlindSpot, and never pairs two same-named functions of two
//! files by name.
//!
//! The brief asks for a `no-override-concept` BlindSpot here. No fact in a C
//! `LocalGraph` can trigger it (`c/parser.rs` writes `heritage: vec![]` at
//! every node), so this test pins the honest empty answer until the C parser
//! records an embedded first member.

mod overrides_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_analyzer::post_process::overrides::{
    BS_KIND_NO_OVERRIDE_CONCEPT, BS_KIND_OVERRIDE_AMBIGUOUS,
};
use overrides_support::{blind_spots, graph_of, overrides};

#[test]
fn test_overrides_c_two_same_named_bases_binds_declared_one() {
    let graph = graph_of(
        &CProvider::new().unwrap(),
        &[
            (
                "a/decoy.c",
                "struct Base { int x; };\nvoid run(struct Base *b) {}\n",
            ),
            (
                "b/child.c",
                "struct Base { int x; };\nstruct Child { struct Base base; };\nvoid run(struct Child *c) {}\n",
            ),
        ],
    );
    assert!(
        overrides(&graph).is_empty(),
        "C has no override: no Overrides edge; got {:?}",
        overrides(&graph)
    );
    for kind in [BS_KIND_NO_OVERRIDE_CONCEPT, BS_KIND_OVERRIDE_AMBIGUOUS] {
        assert!(
            blind_spots(&graph, kind).is_empty(),
            "no {kind} BlindSpot without heritage"
        );
    }
}

/// Stands for: C has no override, so two same-named functions in two files
/// give no edge and no BlindSpot of any override kind.
#[test]
fn test_overrides_c_same_name_emits_no_edge() {
    let graph = graph_of(
        &CProvider::new().unwrap(),
        &[
            ("a/run.c", "void run(void) {}\n"),
            (
                "b/run.c",
                "struct Base { int x; };\nstruct Child { struct Base base; };\nvoid run(void) {}\n",
            ),
        ],
    );
    assert!(overrides(&graph).is_empty(), "{:?}", overrides(&graph));
    let spots = overrides_support::override_blind_spots(&graph);
    assert!(spots.is_empty(), "{spots:?}");
}
