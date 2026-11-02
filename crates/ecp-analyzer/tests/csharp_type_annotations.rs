//! C#: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
mod name_ref_support;

use ecp_analyzer::c_sharp::parser::CSharpProvider;
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
/// return form beside `csharp_partial_parts.rs`. `List<Tok>` is a generic and
/// binds nothing.
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
    assert_eq!(annotated(&g, "All"), Vec::<(String, String)>::new());
}
