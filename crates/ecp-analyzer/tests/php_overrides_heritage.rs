//! `Overrides` in PHP binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The parser captures no `#[\Override]`, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_php_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        ("a/decoy.php", "<?php\nclass Base {\n    public function run() {}\n}\n"),
        ("b/child.php", "<?php\nclass Base {\n    public function run() {}\n}\nclass Child extends Base {\n    public function run() {}\n}\n"),
        "run",
        Evidence::Heuristic,
    );
}

/// Stands for: `use App\Base;` never binds the namesake `Other\Base`. The
/// installed ecp 0.16.4 resolver gives `Child` no `Extends` edge for this
/// fixture (its type tiers do not read PHP `use`; the `Imports` edge does),
/// so the expected answer is one `override-base-ambiguous` BlindSpot with
/// the two candidates. A resolver that reads the `use` binds `App\Base`
/// instead; the test accepts that edge and nothing else.
#[test]
fn test_overrides_php_imported_base_avoids_namesake() {
    use ecp_analyzer::post_process::overrides::BS_KIND_OVERRIDE_BASE_AMBIGUOUS;
    let graph = overrides_support::graph_of(
        &ecp_analyzer::php::parser::PhpProvider::new().unwrap(),
        &[
            (
                "App/Base.php",
                "<?php\nnamespace App;\nclass Base {\n    public function run() {}\n}\n",
            ),
            (
                "Other/Base.php",
                "<?php\nnamespace Other;\nclass Base {\n    public function run() {}\n}\n",
            ),
            (
                "Child.php",
                "<?php\nnamespace Child;\nuse App\\Base;\nclass Child extends Base {\n    public function run() {}\n}\n",
            ),
        ],
    );
    let edges = overrides_support::overrides(&graph);
    let spots = overrides_support::override_blind_spots(&graph);
    assert!(
        edges
            .iter()
            .all(|(_, target, _, _)| target.file != "Other/Base.php"),
        "never the namesake: {edges:?}"
    );
    match edges.as_slice() {
        [] => {
            assert_eq!(spots.len(), 1, "{spots:?}");
            let (kind, file, row, hint) = &spots[0];
            assert_eq!(
                (kind.as_str(), file.as_str(), *row),
                (BS_KIND_OVERRIDE_BASE_AMBIGUOUS, "Child.php", 4)
            );
            assert!(
                hint.contains("`Base`") && hint.contains("2 candidate types"),
                "{hint}"
            );
        }
        [(source, target, _, _)] => {
            assert_eq!(
                (source.file.as_str(), target.file.as_str()),
                ("Child.php", "App/Base.php")
            );
            assert!(spots.is_empty(), "{spots:?}");
        }
        more => panic!("one answer, not {more:?}"),
    }
}
