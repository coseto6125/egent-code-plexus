//! Receiver typing, e2e: the binder types a by-value parameter (`Derived d`).
//! The callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

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
