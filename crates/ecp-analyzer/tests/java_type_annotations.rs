//! Java: `Self` is no declaring-type keyword, so a return type `Self` is an ordinary type name.
mod name_ref_support;

use ecp_analyzer::java::parser::JavaProvider;
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
