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
