//! `Overrides` in JavaScript binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! JavaScript has no override marker, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_javascript_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::javascript::parser::JavaScriptProvider::new().unwrap(),
        ("a/decoy.js", "export class Base {\n  run() {}\n}\n"),
        ("b/child.js", "export class Base {\n  run() {}\n}\nexport class Child extends Base {\n  run() {}\n}\n"),
        "run",
        Evidence::Heuristic,
    );
}
