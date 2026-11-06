//! Kotlin: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
use crate::name_ref_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use name_ref_support::{annotated, graph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: `fun make(): Self` in `class Owner` binds the project class
/// `Self`, never the owner. A language-blind gate would give `Owner`.
#[test]
fn test_self_named_project_class_binds_that_class_not_owner() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[
            ("src/Self.kt", "class Self\n"),
            (
                "src/Owner.kt",
                "class Owner {\n    fun make(): Self = Self()\n}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "make"), vec![pair("src/Self.kt", "Self")]);
}

/// Guard: with no project type `Self`, `fun make(): Self` binds nothing, not the owner.
#[test]
fn test_self_without_project_class_binds_nothing() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/Owner.kt",
            "class Owner {\n    fun make(): Self = TODO()\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "make"), Vec::<(String, String)>::new());
}

/// Contract: `fun find(): Tok?` binds the project class `Tok`, like
/// `fun make(): Tok`. Before the fix the function query captured only a
/// `user_type` return, so a `nullable_type` return had no annotation and
/// no edge.
#[test]
fn test_nullable_return_annotation_binds_project_type() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/app.kt",
            "class Tok\n\nclass Owner {\n    fun find(): Tok? = null\n    fun make(): Tok = Tok()\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "find"), vec![pair("src/app.kt", "Tok")]);
    assert_eq!(annotated(&g, "make"), vec![pair("src/app.kt", "Tok")]);

    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/app.kt",
            "class Owner {\n    fun find(): Tok? = null\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}

/// `List<Tok>` names `Tok` as its element type, so the return binds it.
#[test]
fn test_generic_return_annotation_binds_the_element_type() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/app.kt",
            "class Tok\n\nclass Owner {\n    fun all(): List<Tok> = listOf()\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "all"), [pair("src/app.kt", "Tok")]);
}

/// Contract: a class property (`val` / `var`, nullable or not) and a
/// constructor `val` property typed with a project class reach it, like a
/// parameter does. A builtin, a type parameter, a plain constructor
/// parameter and an untyped property bind nothing.
#[test]
fn test_property_annotation_binds_project_type() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/App.kt",
            "class St\n\
             class C<T>(val st_ctor: St, val n: Int, val tp: T, plain_param: St) {\n    val st_field: St = St()\n    var st_var: St? = null\n    val plain: Int = 1\n    val untyped = 1\n}\n",
        )],
    );
    let st = vec![pair("src/App.kt", "St")];
    assert_eq!(annotated(&g, "st_ctor"), st, "constructor val");
    assert_eq!(annotated(&g, "st_field"), st, "val property");
    assert_eq!(annotated(&g, "st_var"), st, "nullable var property");
    let none = Vec::<(String, String)>::new();
    assert_eq!(annotated(&g, "n"), none, "builtin constructor val");
    assert_eq!(annotated(&g, "tp"), none, "type parameter");
    assert_eq!(annotated(&g, "plain"), none, "builtin property");
    assert_eq!(annotated(&g, "untyped"), none, "no annotation");
    assert_eq!(
        annotated(&g, "plain_param"),
        none,
        "plain parameter is no property"
    );
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::kotlin::parser::KotlinProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(token: &str) -> String {
        format!("src/{token}.kt")
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let files = [
            (token_file(token), format!("class {token}\n")),
            (
                "src/Box.kt".to_string(),
                format!("class Box{params} {{\n    val items: List<{arg}> = listOf()\n}}\n"),
            ),
            (
                "src/Holder.kt".to_string(),
                format!("class Holder {{\n    val tokens: List<{token}> = listOf()\n}}\n"),
            ),
        ];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&KotlinProvider::new().unwrap(), &refs)
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
            (token_file("Token"), "class Token\n".to_string()),
            (
                "src/Box.kt".to_string(),
                "class Box {\n    fun <Token> put(xs: List<Token>) {}\n}\n".to_string(),
            ),
            (
                "src/Holder.kt".to_string(),
                "class Holder {\n    fun take(xs: List<Token>) {}\n}\n".to_string(),
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
        let g = graph(&KotlinProvider::new().unwrap(), &refs);
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
