//! Type annotations on Dart nodes (parameters, properties, top-level vars,
//! return types).
//!
//! Dart uses prefix type syntax (`int x`, `String name`); tree-sitter-dart
//! exposes the `(type ...)` node as a sibling of the identifier rather than
//! a `name:`-prefixed field, so the parser descends positionally. Behavior
//! cross-checked against GitNexus `dart.ts` type-extractor (independent Rust impl).

use crate::name_ref_support;

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use name_ref_support::annotated;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = DartProvider::new().expect("DartProvider init");
    let graph = provider
        .parse_file(Path::new("t.dart"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

// param_* tests removed: formal parameters are no longer emitted as
// Variable nodes (see `fix(analyzer): drop formal_parameter Variable
// emission ...`).

#[test]
fn class_field_int() {
    let nodes = parse(
        r#"class C {
  int counter = 0;
}
"#,
    );
    let p = find(&nodes, "counter", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("int"));
}

#[test]
fn class_field_string() {
    let nodes = parse(
        r#"class C {
  String name = '';
}
"#,
    );
    let p = find(&nodes, "name", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn top_level_variable_with_type() {
    let nodes = parse("double pi = 3.14;\n");
    let v = find(&nodes, "pi", NodeKind::Variable);
    assert_eq!(v.type_annotation.as_deref(), Some("double"));
}

#[test]
fn function_return_type_preserved() {
    // Return type was already captured pre-Wave 3 — assert it still works.
    let nodes = parse("bool f() => true;\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("bool"));
}

/// Contract: `Self make()` where the project declares a type named `Self` binds that
/// type. `Self` is no declaring-type keyword in Dart; a language-blind gate
/// would find no owner and bind nothing.
#[test]
fn test_self_named_project_type_binds_that_type() {
    let g = name_ref_support::graph(
        &DartProvider::new().unwrap(),
        &[(
            "lib/app.dart",
            "class Self {}\n\nclass Owner {\n  Self make() => Self();\n}\n",
        )],
    );
    let edges: Vec<(String, String)> = name_ref_support::refs(
        &g,
        "make",
        ecp_core::graph::RelType::Accesses,
        "type_annotation",
    )
    .into_iter()
    .map(|(file, name, _, _)| (file, name))
    .collect();
    assert_eq!(
        edges,
        vec![("lib/app.dart".to_string(), "Self".to_string())]
    );
}

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: the parser keeps the `?` in the return annotation (`Tok?`), and
/// the reduced key binds the project class `Tok`, like `Tok make()`. Before the
/// change the raw text `Tok?` names no type: no edge for `find`, an edge for
/// `make`. With no project `Tok` (dependency absent) neither binds anything.
#[test]
fn test_nullable_return_annotation_binds_project_type() {
    let src = "class Tok {}\n\nTok? find() => null;\n\nTok make() => Tok();\n";
    let g = name_ref_support::graph(&DartProvider::new().unwrap(), &[("lib/app.dart", src)]);
    assert_eq!(annotated(&g, "find"), [pair("lib/app.dart", "Tok")]);
    assert_eq!(annotated(&g, "make"), [pair("lib/app.dart", "Tok")]);

    let without = "Tok? find() => null;\n";
    let g = name_ref_support::graph(&DartProvider::new().unwrap(), &[("lib/app.dart", without)]);
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}

/// `List<Tok>` names `Tok` as its element type, so the return binds it.
#[test]
fn test_generic_return_annotation_binds_the_element_type() {
    let src = "class Tok {}\n\nList<Tok> all() => [];\n";
    let g = name_ref_support::graph(&DartProvider::new().unwrap(), &[("lib/app.dart", src)]);
    assert_eq!(annotated(&g, "all"), [pair("lib/app.dart", "Tok")]);
}

/// Contract: a field typed with a project class reaches it, nullable or not,
/// and a generic field binds its project type argument, never the wrapper
/// (`List`). A builtin and an untyped field bind nothing. The generic row
/// needs `type_annotation_targets` to read the type arguments of a
/// non-callable's annotation.
#[test]
fn test_field_annotation_binds_project_type_and_generic_argument() {
    let src = "class St {}\n\nclass C {\n  St st_field = St();\n  St? opt;\n  List<St> st_list = [];\n  int plain = 1;\n  var untyped = 1;\n}\n";
    let g = name_ref_support::graph(&DartProvider::new().unwrap(), &[("lib/app.dart", src)]);
    let st = vec![pair("lib/app.dart", "St")];
    assert_eq!(annotated(&g, "st_field"), st, "plain field");
    assert_eq!(annotated(&g, "opt"), st, "nullable field");
    assert_eq!(annotated(&g, "st_list"), st, "generic field binds St only");
    let none = Vec::<(String, String)>::new();
    assert_eq!(annotated(&g, "plain"), none, "builtin");
    assert_eq!(annotated(&g, "untyped"), none, "no annotation");
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::dart::parser::DartProvider;
    use ecp_core::graph::ZeroCopyGraph;

    const FILE: &str = "lib/app.dart";

    fn pair(name: &str) -> (String, String) {
        (FILE.to_string(), name.to_string())
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let src = format!(
            "class {token} {{}}\nclass Box{params} {{\n  List<{arg}> items = [];\n}}\nclass Holder {{\n  List<{token}> tokens = [];\n}}\n"
        );
        graph(&DartProvider::new().unwrap(), &[(FILE, src.as_str())])
    }

    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    /// Stands for: a field whose type argument names a class type parameter
    /// binds no project type although a project class has the same name; the
    /// sibling field in an unparameterised holder (POSITIVE CONTROL) binds it.
    #[test]
    fn test_field_annotation_class_type_parameter_named_like_project_class_binds_nothing() {
        let g = build(Some("Token"), "Token");
        assert!(
            !names(&g, "items").iter().any(|n| n == "Token"),
            "{:?}",
            annotated(&g, "items")
        );
        assert!(
            annotated(&g, "tokens").contains(&pair("Token")),
            "control: {:?}",
            annotated(&g, "tokens")
        );
    }

    /// Stands for: a generic argument in a holder with no type parameters
    /// binds the project class it names.
    #[test]
    fn test_field_annotation_project_class_in_unparameterised_holder_binds_class() {
        let g = build(Some("Token"), "Token");
        assert!(annotated(&g, "tokens").contains(&pair("Token")));
    }

    /// Stands for (M07, rename the parameter): `Box<E>` / `List<E>` binds no
    /// project edge, and `tokens` still binds `Token`.
    #[test]
    fn test_field_annotation_renamed_class_type_parameter_binds_nothing() {
        let g = build(Some("E"), "Token");
        assert!(!names(&g, "items").iter().any(|n| n == "Token"));
        assert!(annotated(&g, "tokens").contains(&pair("Token")));
    }

    /// Stands for (M07, rename the project class): the project class is `Tok`
    /// and the parameter stays `Token`; `items` binds neither.
    #[test]
    fn test_field_annotation_renamed_project_class_keeps_parameter_binding_nothing() {
        let g = build(Some("Token"), "Tok");
        assert!(!names(&g, "items")
            .iter()
            .any(|n| n == "Tok" || n == "Token"));
        assert!(annotated(&g, "tokens").contains(&pair("Tok")));
    }

    /// Stands for (M07, remove the parameter): with no class type parameter
    /// the same field binds the project class.
    #[test]
    fn test_field_annotation_without_class_type_parameter_binds_project_class() {
        let g = build(None, "Token");
        assert!(annotated(&g, "items").contains(&pair("Token")));
    }
}

/// A15b: a top-level variable's annotation reduces like a field's.
mod a15b {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::dart::parser::DartProvider;
    use ecp_core::graph::ZeroCopyGraph;

    const SRC: &str = "class Item {}\n\nList<Item> items = [];\nList<int> numbers = [];\nItem single = Item();\nItem? maybe;\nint count = 0;\n\nclass C {\n  List<Item> field_items = [];\n}\n";

    fn item() -> (String, String) {
        ("lib/app.dart".to_string(), "Item".to_string())
    }

    fn build() -> ZeroCopyGraph {
        graph(&DartProvider::new().unwrap(), &[("lib/app.dart", SRC)])
    }

    /// Stands for: `List<Item> items` at top level binds the argument `Item`
    /// and never the wrapper `List`, as the field form does.
    #[test]
    fn test_top_level_variable_annotation_generic_binds_element_type_only() {
        let g = build();
        assert_eq!(annotated(&g, "items"), vec![item()]);
        // POSITIVE CONTROL: the field form and the plain form.
        assert_eq!(annotated(&g, "field_items"), vec![item()]);
        assert_eq!(annotated(&g, "single"), vec![item()]);
    }

    /// Stands for: builtins (`List<int>`, `int`) bind no project type.
    #[test]
    fn test_top_level_variable_annotation_builtin_generic_binds_nothing() {
        let g = build();
        assert_eq!(annotated(&g, "numbers"), Vec::<(String, String)>::new());
        assert_eq!(annotated(&g, "count"), Vec::<(String, String)>::new());
        assert!(!annotated(&g, "items").iter().any(|(_, n)| n == "List"));
    }

    /// Stands for: a nullable top-level `Item? maybe` binds the project class.
    #[test]
    fn test_top_level_variable_annotation_nullable_binds_project_type() {
        let g = build();
        assert_eq!(annotated(&g, "maybe"), vec![item()]);
    }

    /// Stands for (M13): the same type text yields the same `annotated` set at
    /// top level and as a class field.
    #[test]
    fn test_metamorphic_top_level_and_field_type_text_yield_same_edges() {
        let g = build();
        assert_eq!(annotated(&g, "items"), annotated(&g, "field_items"));
        assert!(!annotated(&g, "items").is_empty());
    }
}
