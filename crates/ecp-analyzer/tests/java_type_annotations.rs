//! Java: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
use crate::name_ref_support;

use ecp_analyzer::java::parser::JavaProvider;
use name_ref_support::{annotated, graph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: `Self make()` in `class Owner` binds the project class `Self`,
/// never the owner. A language-blind gate would give `Owner`.
#[test]
fn test_self_named_project_class_binds_that_class_not_owner() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("src/Self.java", "public class Self {}\n"),
            (
                "src/Owner.java",
                "public class Owner {\n  public Self make() { return null; }\n}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "make"), vec![pair("src/Self.java", "Self")]);
}

/// Guard: with no project type `Self`, `Self make()` binds nothing, not the owner.
#[test]
fn test_self_without_project_class_binds_nothing() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[(
            "src/Owner.java",
            "public class Owner {\n  public Self make() { return null; }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "make"), Vec::<(String, String)>::new());
}

/// Java has no nullable or pointer wrapper. A plain `Tok` return binds the
/// project class, and `List<Tok>` binds it as its element type.
#[test]
fn test_plain_and_generic_return_bind_the_project_type() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("src/Tok.java", "public class Tok {}\n"),
            (
                "src/Owner.java",
                "public class Owner {\n  public Tok make() { return null; }\n  public java.util.List<Tok> all() { return null; }\n}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "make"), vec![pair("src/Tok.java", "Tok")]);
    assert_eq!(annotated(&g, "all"), vec![pair("src/Tok.java", "Tok")]);
}

/// Contract: a field typed with a project class reaches it, and a generic
/// field binds its project type argument, never the wrapper (`List`). A
/// builtin and a type parameter of the enclosing class bind nothing.
/// Needs `type_annotation_targets` to read the type arguments of a
/// non-callable's annotation; before that, `List<St>` looked up the whole
/// text and bound nothing.
#[test]
fn test_field_annotation_binds_project_type_and_generic_argument() {
    let g = graph(
        &JavaProvider::new().unwrap(),
        &[
            ("src/St.java", "class St {}\n"),
            (
                "src/C.java",
                "import java.util.List;\n\
                 class C<T> {\n    St st_field;\n    List<St> st_list_field;\n    int plain;\n    T tparam;\n    List<String> strings;\n}\n",
            ),
        ],
    );
    let st = vec![pair("src/St.java", "St")];
    assert_eq!(annotated(&g, "st_field"), st, "plain field");
    assert_eq!(
        annotated(&g, "st_list_field"),
        st,
        "generic field binds St only"
    );
    let none = Vec::<(String, String)>::new();
    assert_eq!(annotated(&g, "plain"), none, "builtin");
    assert_eq!(annotated(&g, "tparam"), none, "type parameter");
    assert_eq!(annotated(&g, "strings"), none, "generic of builtins");
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::java::parser::JavaProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(token: &str) -> String {
        format!("src/{token}.java")
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let files = [
            (token_file(token), format!("class {token} {{}}\n")),
            (
                "src/Box.java".to_string(),
                format!(
                    "import java.util.List;\nclass Box{params} {{\n    List<{arg}> items;\n}}\n"
                ),
            ),
            (
                "src/Holder.java".to_string(),
                format!("import java.util.List;\nclass Holder {{\n    List<{token}> tokens;\n}}\n"),
            ),
        ];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&JavaProvider::new().unwrap(), &refs)
    }

    /// Stands for: a field whose type argument names a class type parameter
    /// binds no project type, although a project class has the same name; the
    /// sibling field in an unparameterised holder (POSITIVE CONTROL) binds it.
    #[test]
    fn test_field_annotation_class_type_parameter_named_like_project_class_binds_nothing() {
        let g = build(Some("Token"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "items binds a type parameter as a project class: {:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for: a generic argument in a holder with no type parameters
    /// binds the project class it names.
    #[test]
    fn test_field_annotation_project_class_in_unparameterised_holder_binds_class() {
        let g = build(Some("Token"), "Token");
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the parameter): `Box<E>` / `List<E>` binds no
    /// project edge, and `tokens` still binds `Token`.
    #[test]
    fn test_field_annotation_renamed_class_type_parameter_binds_nothing() {
        let g = build(Some("E"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, rename the project class): the project class is `Tok`
    /// and the parameter stays `Token`; `items` binds neither, `tokens` binds `Tok`.
    #[test]
    fn test_field_annotation_renamed_project_class_keeps_parameter_binding_nothing() {
        let g = build(Some("Token"), "Tok");
        let items = names(&g, "items");
        assert!(
            !items.iter().any(|n| n == "Tok" || n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Tok"), "Tok")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M07, remove the parameter): with no class type parameter
    /// the same field binds the project class `Token`.
    #[test]
    fn test_field_annotation_without_class_type_parameter_binds_project_class() {
        let g = build(None, "Token");
        assert!(
            annotated(&g, "items").contains(&pair(&token_file("Token"), "Token")),
            "{:?}",
            annotated(&g, "items")
        );
    }

    fn method_files() -> Vec<(String, String)> {
        vec![
            (token_file("Token"), "class Token {}\n".to_string()),
            (
                "src/Box.java".to_string(),
                "import java.util.List;\nclass Box {\n    <Token> void put(List<Token> xs) {}\n}\n"
                    .to_string(),
            ),
            (
                "src/Holder.java".to_string(),
                "import java.util.List;\nclass Holder {\n    void take(List<Token> xs) {}\n}\n"
                    .to_string(),
            ),
        ]
    }

    /// Stands for: a method type parameter named like a project class binds
    /// nothing for `put`; `take`, declared where no such parameter exists
    /// (POSITIVE CONTROL), binds the project class.
    #[test]
    fn test_field_annotation_method_type_parameter_named_like_project_class_binds_nothing() {
        let files = method_files();
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        let g = graph(&JavaProvider::new().unwrap(), &refs);
        assert!(
            !names(&g, "put").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "put")
        );
        assert!(
            annotated(&g, "take").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "take")
        );
    }

    /// Stands for: an inner class's own type parameter shadows a project class
    /// for its field, while the outer class's field (`tokens`, which sees no
    /// `Token` parameter) binds the class.
    #[test]
    fn test_field_annotation_nested_class_type_parameter_shadows_outer_binds_nothing() {
        let g = graph(
            &JavaProvider::new().unwrap(),
            &[
                (token_file("Token").as_str(), "class Token {}\n"),
                (
                    "src/Outer.java",
                    "import java.util.List;\nclass Outer<T> {\n    class Inner<Token> {\n        List<Token> items;\n    }\n    List<Token> tokens;\n}\n",
                ),
            ],
        );
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for (M12): a method type parameter beside a class type
    /// parameter, both named `Token` or one named `E`, binds nothing for `put`.
    #[test]
    fn test_metamorphic_method_type_parameter_beside_class_type_parameter_binds_nothing() {
        let g = graph(
            &JavaProvider::new().unwrap(),
            &[
                (token_file("Token").as_str(), "class Token {}\n"),
                (
                    "src/Box.java",
                    "import java.util.List;\nclass Box<Token> {\n    <Token> void put(List<Token> xs) {}\n}\n",
                ),
                (
                    "src/Box2.java",
                    "import java.util.List;\nclass Box2<E> {\n    <Token> void put(List<Token> xs) {}\n}\n",
                ),
                (
                    "src/Holder.java",
                    "import java.util.List;\nclass Holder {\n    void take(List<Token> xs) {}\n}\n",
                ),
            ],
        );
        assert!(
            !names(&g, "put").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "put")
        );
        assert!(
            annotated(&g, "take").contains(&pair(&token_file("Token"), "Token")),
            "control: {:?}",
            annotated(&g, "take")
        );
    }
}
