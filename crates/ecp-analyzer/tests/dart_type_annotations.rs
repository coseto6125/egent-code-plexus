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
