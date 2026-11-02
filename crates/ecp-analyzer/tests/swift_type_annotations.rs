//! Type annotations on Swift nodes (parameters, properties, vars).
//!
//! Swift uses postfix type syntax `name: Type` (vs C's prefix `Type name`),
//! so the parser reads the type-annotation node text directly rather than
//! slicing source-before-the-name. Behavior cross-checked against GitNexus
//! `swift.ts` type-extractor (independent Rust impl).

mod name_ref_support;

use ecp_analyzer::swift::parser::SwiftProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::{NodeKind, RelType};
use name_ref_support::annotated;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = SwiftProvider::new().expect("SwiftProvider init");
    let graph = provider
        .parse_file(Path::new("t.swift"), src.as_bytes())
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
fn class_var_property_type() {
    let nodes = parse(
        r#"class C {
            var counter: Int = 0
        }"#,
    );
    let p = find(&nodes, "counter", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("Int"));
}

#[test]
fn class_let_property_type() {
    let nodes = parse(
        r#"class C {
            let name: String
        }"#,
    );
    let p = find(&nodes, "name", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("String"));
}

#[test]
fn top_level_let_with_type() {
    // Top-level `let` is a Variable, not a Property (which is class/struct scope).
    let nodes = parse("let pi: Double = 3.14\n");
    let p = find(&nodes, "pi", NodeKind::Variable);
    assert_eq!(p.type_annotation.as_deref(), Some("Double"));
}

#[test]
fn class_var_untyped_emits_property() {
    let nodes = parse("class C { var x = 0 }\n");
    find(&nodes, "x", NodeKind::Property);
}

#[test]
fn class_optional_type_property() {
    let nodes = parse("class C { var x: Int? }\n");
    let p = find(&nodes, "x", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("Int?"));
}

#[test]
fn class_array_type_property() {
    let nodes = parse("class C { var items: [String] }\n");
    let p = find(&nodes, "items", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("[String]"));
}

#[test]
fn class_closure_type_property() {
    let nodes = parse("class C { var handler: (Int) -> Bool }\n");
    let p = find(&nodes, "handler", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("(Int) -> Bool"));
}

#[test]
fn tuple_pattern_emits_both_names() {
    let nodes = parse("class C { let (a, b) = (1, 2) }\n");
    find(&nodes, "a", NodeKind::Property);
    find(&nodes, "b", NodeKind::Property);
}

#[test]
fn init_emits_constructor() {
    let nodes = parse("class C { init() {} }\n");
    find(&nodes, "init", NodeKind::Constructor);
}

#[test]
fn init_with_params_emits_constructor() {
    let nodes = parse("class C { init(x: Int, y: String) {} }\n");
    find(&nodes, "init", NodeKind::Constructor);
}

/// FU-2026-10-30-c0bc9a616111 part A: `Self` stands for the type that owns
/// the property, so `ecp impact` on the class lists the property.
#[test]
fn test_self_property_annotation_binds_owner_type() {
    let g = name_ref_support::graph(
        &SwiftProvider::new().unwrap(),
        &[(
            "Sources/Resolver.swift",
            "class Resolver {\n    var twin: Self?\n    var plain: Self\n}\n",
        )],
    );
    for property in ["twin", "plain"] {
        let edges: Vec<(String, String)> =
            name_ref_support::refs(&g, property, RelType::Accesses, "type_annotation")
                .into_iter()
                .map(|(file, name, _, _)| (file, name))
                .collect();
        assert_eq!(
            edges,
            [("Sources/Resolver.swift".to_string(), "Resolver".to_string())],
            "{property}"
        );
    }
}

fn pair(file: &str, name: &str) -> (String, String) {
    (file.to_string(), name.to_string())
}

/// Contract: a property typed `Tok?` binds the project class `Tok`, like
/// `var plain: Tok`. The parser keeps the `?` (`class_optional_type_property`).
/// Swift functions carry no return annotation, so the property is the form to
/// test. Before the change the raw text `Tok?` names no type: no edge for
/// `maybe`. With no project `Tok` it binds nothing.
#[test]
fn test_optional_property_annotation_binds_project_type() {
    let src = "class Tok {}\nclass Owner {\n    var maybe: Tok?\n    var plain: Tok\n}\n";
    let g = name_ref_support::graph(
        &SwiftProvider::new().unwrap(),
        &[("Sources/App.swift", src)],
    );
    assert_eq!(annotated(&g, "maybe"), [pair("Sources/App.swift", "Tok")]);
    assert_eq!(annotated(&g, "plain"), [pair("Sources/App.swift", "Tok")]);

    let g = name_ref_support::graph(
        &SwiftProvider::new().unwrap(),
        &[(
            "Sources/App.swift",
            "class Owner {\n    var maybe: Tok?\n}\n",
        )],
    );
    assert_eq!(annotated(&g, "maybe"), Vec::<(String, String)>::new());
}

/// Guard: `[Tok]` and `Array<Tok>` are generics; `return_type_key` gives
/// `None`, so they bind nothing, before and after the change.
#[test]
fn test_generic_property_annotation_binds_nothing() {
    let src = "class Tok {}\nclass Owner {\n    var items: [Tok]\n    var more: Array<Tok>\n}\n";
    let g = name_ref_support::graph(
        &SwiftProvider::new().unwrap(),
        &[("Sources/App.swift", src)],
    );
    assert_eq!(annotated(&g, "items"), Vec::<(String, String)>::new());
    assert_eq!(annotated(&g, "more"), Vec::<(String, String)>::new());
}
