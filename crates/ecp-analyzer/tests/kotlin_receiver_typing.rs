//! Receiver typing, e2e: the binder types a parameter (`d: Derived`). The
//! callee's method is inherited from a base type in another file, and an
//! unrelated decoy type declares a method of the same name. The ladder must
//! resolve the typed callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = KotlinProvider::new().expect("KotlinProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.kt",
                r#"open class Base {
    fun greet(): Int {
        return 1
    }
}
"#,
            ),
            (
                "src/Derived.kt",
                r#"class Derived : Base()
"#,
            ),
            (
                "src/Decoy.kt",
                r#"class Decoy {
    fun greet(): Int {
        return 2
    }
}
"#,
            ),
            (
                "src/App.kt",
                r#"fun run(d: Derived): Int {
    return d.greet()
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/Base.kt");
}
