//! C#: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
use crate::name_ref_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use name_ref_support::{annotated, graph};

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: `Self Make()` in `class Owner` binds the project class `Self`,
/// never the owner. A language-blind gate would give `Owner`.
#[test]
fn test_self_named_project_class_binds_that_class_not_owner() {
    let g = graph(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Self.cs", "public class Self {}\n"),
            (
                "src/Owner.cs",
                "public class Owner {\n    public Self Make() { return null; }\n}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "Make"), vec![pair("src/Self.cs", "Self")]);
}

/// Guard: with no project type `Self`, `Self Make()` binds nothing, not the owner.
#[test]
fn test_self_without_project_class_binds_nothing() {
    let g = graph(
        &CSharpProvider::new().unwrap(),
        &[(
            "src/Owner.cs",
            "public class Owner {\n    public Self Make() { return null; }\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "Make"), Vec::<(String, String)>::new());
}

/// Guard: C# already reduces the key (`Tok?` to `Tok`), so a nullable return
/// binds the project class both before and after the change; this adds the
/// return form beside `csharp_partial_parts.rs`. `List<Tok>` binds `Tok` as its
/// element type.
#[test]
fn test_nullable_return_annotation_binds_project_type() {
    let g = graph(
        &CSharpProvider::new().unwrap(),
        &[
            ("src/Tok.cs", "public class Tok {}\n"),
            (
                "src/Owner.cs",
                "public class Owner {\n    public Tok? Find() { return null; }\n    public System.Collections.Generic.List<Tok> All() { return null; }\n}\n",
            ),
        ],
    );
    assert_eq!(annotated(&g, "Find"), vec![pair("src/Tok.cs", "Tok")]);
    assert_eq!(annotated(&g, "All"), vec![pair("src/Tok.cs", "Tok")]);
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::c_sharp::parser::CSharpProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(token: &str) -> String {
        format!("src/{token}.cs")
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let files = [(token_file(token), format!("class {token} {{}}\n")),
            (
                "src/Box.cs".to_string(),
                format!("using System.Collections.Generic;\nclass Box{params} {{\n    List<{arg}> items;\n}}\n"),
            ),
            (
                "src/Holder.cs".to_string(),
                format!("using System.Collections.Generic;\nclass Holder {{\n    List<{token}> tokens;\n}}\n"),
            )];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&CSharpProvider::new().unwrap(), &refs)
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
                "src/Box.cs".to_string(),
                "using System.Collections.Generic;\nclass Box {\n    void put<Token>(List<Token> xs) {}\n}\n".to_string(),
            ),
            (
                "src/Holder.cs".to_string(),
                "using System.Collections.Generic;\nclass Holder {\n    void take(List<Token> xs) {}\n}\n".to_string(),
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
        let g = graph(&CSharpProvider::new().unwrap(), &refs);
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
