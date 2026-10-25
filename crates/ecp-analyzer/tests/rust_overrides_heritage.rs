//! `Overrides` in Rust binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! The `impl Base for Child` block names the trait, so the method implements that trait's `run`.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_rust_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        ("a/decoy.rs", "pub trait Base {\n    fn run(&self) {}\n}\n"),
        ("b/child.rs", "pub trait Base {\n    fn run(&self) {}\n}\npub struct Child;\nimpl Base for Child {\n    fn run(&self) {}\n}\n"),
        "run",
        Evidence::Exact,
    );
}

/// Stands for: a Rust impl method implements only the trait its `impl`
/// block names; a supertrait that declares the same name gets no edge.
#[test]
fn test_overrides_rust_trait_impl_stays_in_named_trait() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::rust::parser::RustProvider::new().unwrap(),
        &[(
            "lib.rs",
            "pub trait Super {\n    fn m(&self) {}\n}\npub trait T: Super {\n    fn m(&self) {}\n}\npub struct S;\nimpl T for S {\n    fn m(&self) {}\n}\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 1, "S.m -> T.m only; got {edges:?}");
    assert_eq!(
        (edges[0].0.line, edges[0].1.line),
        (8, 4),
        "never Super.m (row 1)"
    );
    overrides_support::assert_evidence(&edges[0], Evidence::Exact);
    assert!(overrides_support::override_blind_spots(&graph).is_empty());
}
