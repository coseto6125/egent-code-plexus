//! Receiver typing, e2e: the binder types a by-value parameter (`Derived d`).
//! The callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

use crate::receiver_typing_support;

use ecp_analyzer::cpp::parser::CppProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = CppProvider::new().expect("CppProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/base.hpp",
                r#"class Base {
public:
    int greet() { return 1; }
};
"#,
            ),
            (
                "src/derived.hpp",
                r#"#include "base.hpp"

class Derived : public Base {
};
"#,
            ),
            (
                "src/decoy.hpp",
                r#"class Decoy {
public:
    int greet() { return 2; }
};
"#,
            ),
            (
                "src/app.cpp",
                r#"#include "derived.hpp"

int run(Derived d) {
    return d.greet();
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/base.hpp");
}

/// Rule U: in C++, a member call on an untyped receiver that binds a method by
/// its name alone is a guess: 0.55, `call:untyped-member`, from any tier.
/// The caller's own type and an ambiguous name bind nothing; the nearest
/// copy of a duplicated declaration keeps 0.6.
#[test]
fn test_untyped_member_call_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("cpp");
}
