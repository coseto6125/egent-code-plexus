//! `Overrides` in Kotlin binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! `override` marks the method.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_kotlin_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::kotlin::parser::KotlinProvider::new().unwrap(),
        ("a/Base.kt", "package a\nopen class Base {\n    open fun run() {}\n}\n"),
        ("b/Child.kt", "package b\nopen class Base {\n    open fun run() {}\n}\nclass Child : Base() {\n    override fun run() {}\n}\n"),
        "run",
        Evidence::Exact,
    );
}
