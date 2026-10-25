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
