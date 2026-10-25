//! `Overrides` in Go (slice P7). Go has no method override: a method of a
//! struct that embeds `Base` shadows the promoted `Base` method. The pass
//! emits no `Overrides` edge and records a `no-override-concept` BlindSpot
//! at the shadowing method, so "who overrides `Base.Run`" answers empty with
//! a reason. The embedded `Base` is the one of the struct's own file, never
//! the same-named `Base` of another package.

mod overrides_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_analyzer::post_process::overrides::BS_KIND_NO_OVERRIDE_CONCEPT;
use overrides_support::{blind_spots, graph_of, overrides};

const DECOY: (&str, &str) = (
    "a/decoy.go",
    "package a\n\ntype Base struct{}\n\nfunc (b Base) Run() {}\n",
);

const CHILD: (&str, &str) = (
    "b/child.go",
    "package b\n\ntype Base struct{}\n\nfunc (b Base) Run() {}\n\ntype Child struct {\n\tBase\n}\n\nfunc (c Child) Run() {}\n",
);

#[test]
fn test_overrides_go_two_same_named_bases_binds_declared_one() {
    let graph = graph_of(&GoProvider::new().unwrap(), &[DECOY, CHILD]);
    assert!(
        overrides(&graph).is_empty(),
        "Go has no override: no Overrides edge; got {:?}",
        overrides(&graph)
    );
    let spots = blind_spots(&graph, BS_KIND_NO_OVERRIDE_CONCEPT);
    assert_eq!(
        spots.len(),
        1,
        "one {BS_KIND_NO_OVERRIDE_CONCEPT} BlindSpot, at Child.Run; got {spots:?}"
    );
    let (file, row, hint) = &spots[0];
    assert_eq!(
        file, CHILD.0,
        "the shadowing method's file, never the decoy's"
    );
    assert_eq!(*row, 10, "`func (c Child) Run()` is line 11");
    assert!(
        hint.contains("`Run`") && hint.contains("`Base`"),
        "the hint names the method and the embedded type; got {hint}"
    );
}

#[test]
fn test_overrides_go_method_without_embedded_namesake_records_nothing() {
    let graph = graph_of(
        &GoProvider::new().unwrap(),
        &[(
            "b/child.go",
            "package b\n\ntype Base struct{}\n\nfunc (b Base) Stop() {}\n\ntype Child struct {\n\tBase\n}\n\nfunc (c Child) Run() {}\n",
        )],
    );
    assert!(overrides(&graph).is_empty());
    assert!(
        blind_spots(&graph, BS_KIND_NO_OVERRIDE_CONCEPT).is_empty(),
        "`Run` shadows nothing, so no BlindSpot"
    );
}

/// Stands for: Go records the embedding shadow as `no-override-concept`
/// and nothing else — no edge, and no override-lookup gap for an embedded
/// type outside the graph.
#[test]
fn test_overrides_go_embedded_shadow_reports_no_concept() {
    let graph = graph_of(
        &GoProvider::new().unwrap(),
        &[(
            "b/child.go",
            "package b\n\nimport \"sync\"\n\ntype Base struct{}\n\nfunc (b Base) Run() {}\n\ntype Child struct {\n\tBase\n\tsync.Mutex\n}\n\nfunc (c Child) Run() {}\n",
        )],
    );
    assert!(overrides(&graph).is_empty(), "{:?}", overrides(&graph));
    let spots = overrides_support::override_blind_spots(&graph);
    assert_eq!(spots.len(), 1, "{spots:?}");
    let (kind, file, row, hint) = &spots[0];
    assert_eq!(
        (kind.as_str(), file.as_str(), *row),
        (BS_KIND_NO_OVERRIDE_CONCEPT, "b/child.go", 13)
    );
    assert!(hint.contains("`Run`") && hint.contains("`Base`"), "{hint}");
}
