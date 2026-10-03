//! Receiver typing, e2e: the binder types a parameter (`d: Derived`). The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::swift::parser::SwiftProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = SwiftProvider::new().expect("SwiftProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "Sources/Base.swift",
                r#"class Base {
    func greet() -> Int {
        return 1
    }
}
"#,
            ),
            (
                "Sources/Derived.swift",
                r#"class Derived: Base {
}
"#,
            ),
            (
                "Sources/Decoy.swift",
                r#"class Decoy {
    func greet() -> Int {
        return 2
    }
}
"#,
            ),
            (
                "Sources/App.swift",
                r#"func run(d: Derived) -> Int {
    return d.greet()
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(
        graphs,
        "run",
        "Derived.greet",
        "greet",
        "Sources/Base.swift",
    );
}
