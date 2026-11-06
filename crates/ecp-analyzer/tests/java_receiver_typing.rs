//! Receiver typing, e2e: the binder types a formal parameter (`Derived d`). The
//! classes share one package, so no import binds the qualifier. The callee's
//! method is inherited from a base type in another file, and an unrelated decoy
//! type declares a method of the same name. The ladder must resolve the typed
//! callee through the type's declared heritage to the base.

use crate::receiver_typing_support;

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

/// Gate for FU 755942c857a3: only a C# class drops an interface's member
/// from its lookup. A Java class inherits an interface's `default` method,
/// so `d.greet()` on `Derived implements Greeter` still binds
/// `Greeter.greet`. Passes before the fix as well; an ungated drop would
/// leave no edge.
#[test]
fn test_typed_param_interface_default_method_resolves_to_interface() {
    let provider = JavaProvider::new().expect("JavaProvider::new");
    let graphs = parse_all(
        &provider,
        &[
            (
                "src/Greeter.java",
                r#"public interface Greeter {
    default int greet() {
        return 1;
    }
}
"#,
            ),
            (
                "src/Base.java",
                r#"public class Base {
}
"#,
            ),
            (
                "src/Derived.java",
                r#"public class Derived extends Base implements Greeter {
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
    assert_single_call_into_base(graphs, "run", "Derived.greet", "greet", "src/Greeter.java");
}

/// Rule U: in Java, a member call on an untyped receiver that binds a method by
/// its name alone is a guess: 0.55, `call:untyped-member`, from any tier.
/// The caller's own type and an ambiguous name bind nothing; the nearest
/// copy of a duplicated declaration keeps 0.6.
#[test]
fn test_untyped_member_call_binds_by_name_as_tagged_guess() {
    receiver_typing_support::assert_untyped_member_is_guess("java");
}

/// Stands for (M01, Java): a same-named Kotlin class added to the project
/// leaves the Java typed call on its own file, and no edge reaches the added
/// file.
#[test]
fn test_calls_java_typed_receiver_with_kotlin_namesake_binds_java_method() {
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    use ecp_analyzer::resolution::builder::GraphBuilder;
    use ecp_core::analyzer::provider::LanguageProvider;
    use std::path::Path;

    let java = JavaProvider::new().unwrap();
    let build = |with_kotlin: bool| {
        let mut builder = GraphBuilder::new();
        for (path, src) in [
            ("Cls.java", "class Cls { void m() {} }\n"),
            ("Use.java", "class Use { void run(Cls c) { c.m(); } }\n"),
        ] {
            builder.add_graph(java.parse_file(Path::new(path), src.as_bytes()).unwrap());
        }
        if with_kotlin {
            builder.add_graph(
                KotlinProvider::new()
                    .unwrap()
                    .parse_file(Path::new("Cls.kt"), b"class Cls { fun m() {} }\n")
                    .unwrap(),
            );
        }
        builder.build()
    };
    let base = receiver_typing_support::call_rows_to(&build(false), "run", "m");
    assert_eq!(base.len(), 1, "{base:?}");
    assert_eq!(
        (base[0].0.as_str(), base[0].2.as_str()),
        ("Cls.java", "call")
    );
    let variant = receiver_typing_support::call_rows_to(&build(true), "run", "m");
    assert_eq!(
        variant, base,
        "the added Cls.kt must not change the verdict"
    );
}
