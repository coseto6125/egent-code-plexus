//! Kotlin: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
mod name_ref_support;

use ecp_analyzer::kotlin::parser::KotlinProvider;
use ecp_core::graph::{RelType, ZeroCopyGraph};
use name_ref_support::{graph, refs};

/// `(target file, target name)` of every `type_annotation` edge out of `source`.
fn annotated(g: &ZeroCopyGraph, source: &str) -> Vec<(String, String)> {
    refs(g, source, RelType::Accesses, "type_annotation")
        .into_iter()
        .map(|(file, name, _, _)| (file, name))
        .collect()
}

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
/// `fun make(): Tok`. The `Tok?` text reaches the edge only if the
/// `(user_type)? @type` capture keeps the `?`; the sibling test
/// `test_resolve_call_kotlin_nullable_return_chain_binds_return_type_member`
/// shows the annotation is kept as `Foo?`. Before the change the raw text
/// `Tok?` names no type: no edge. Not run here: confirm it is red first.
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

/// Guard: `List<Tok>` is a generic; `return_type_key` gives `None`, so it
/// binds nothing, before and after the change.
#[test]
fn test_generic_return_annotation_binds_nothing() {
    let g = graph(
        &KotlinProvider::new().unwrap(),
        &[(
            "src/app.kt",
            "class Tok\n\nclass Owner {\n    fun all(): List<Tok> = listOf()\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "all"), Vec::<(String, String)>::new());
}
