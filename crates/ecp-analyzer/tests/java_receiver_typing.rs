//! Receiver typing, e2e: the binder types a formal parameter (`Derived d`). The
//! classes share one package, so no import binds the qualifier. The callee's
//! method is inherited from a base type in another file, and an unrelated decoy
//! type declares a method of the same name. The ladder must resolve the typed
//! callee through the type's declared heritage to the base.

mod receiver_typing_support;

use ecp_analyzer::java::parser::JavaProvider;
use receiver_typing_support::{assert_single_call_into_base, parse_all};

#[test]
fn test_typed_param_inherited_method_resolves_to_base_class() {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.java",
                r#"public class Base {
    public int greet() {
        return 1;
    }
}
"#,
            ),
            (
                "src/Derived.java",
                r#"public class Derived extends Base {
}
"#,
            ),
            (
                "src/Decoy.java",
                r#"public class Decoy {
    public int greet() {
        return 2;
    }
}
"#,
            ),
            (
                "src/App.java",
                r#"public class App {
    public int run(Derived d) {
        return d.greet();
    }
}
"#,
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/Base.java");
}

/// Java keeps fields and methods in separate namespaces: a field `greet` on
/// `Derived` does not hide the inherited `greet()`.
#[test]
fn test_same_named_field_does_not_hide_inherited_method() {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Base.java",
                "public class Base {\n    public int greet() {\n        return 1;\n    }\n}\n",
            ),
            (
                "src/Derived.java",
                "public class Derived extends Base {\n    int greet;\n}\n",
            ),
            (
                "src/Decoy.java",
                "public class Decoy {\n    public int greet() {\n        return 2;\n    }\n}\n",
            ),
            (
                "src/App.java",
                "public class App {\n    public int run(Derived d) {\n        return d.greet();\n    }\n}\n",
            ),
        ],
    );
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/Base.java");
}
