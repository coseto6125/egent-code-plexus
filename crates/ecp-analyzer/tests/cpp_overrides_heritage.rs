//! `Overrides` in C++ binds the declared base (slice P7): two types
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
fn test_overrides_cpp_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::cpp::parser::CppProvider::new().unwrap(),
        ("a/decoy.hpp", "struct Base {\n    virtual void run() {}\n};\n"),
        ("b/child.cpp", "struct Base {\n    virtual void run() {}\n};\nstruct Child : public Base {\n    void run() override {}\n};\n"),
        "run",
        Evidence::Exact,
    );
}
