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
