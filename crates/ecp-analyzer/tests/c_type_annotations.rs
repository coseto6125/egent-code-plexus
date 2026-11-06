//! Type annotations on C nodes (params, fields, return types, top-level vars).
//!
//! Covers Wave 2 task D3 from
//! `docs/specs/2026-05-15-language-coverage-gaps.md`.
//!
//! Conventions documented in `src/c/parser.rs::slice_type_before`:
//! - **Pointer spacing**: preserved as-written. `char* s` → `"char*"`,
//!   `char * s` → `"char *"`. Source is source of truth.
//! - **Qualifier inclusion**: full prefix including storage-class. E.g.
//!   `static const int N` → `"static const int"`.

use crate::name_ref_support;

use ecp_analyzer::c::parser::CProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use name_ref_support::annotated;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CProvider::new().expect("CProvider init");
    let graph = provider
        .parse_file(Path::new("t.c"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

// param_primitive / param_pointer / param_const_pointer removed: formal
// parameters are no longer emitted as Variable nodes (see commit log on
// `fix(analyzer): drop formal_parameter Variable emission ...`). Parameter
// semantics live on the Function node itself, not as separate Variables.

#[test]
fn return_type_int() {
    let nodes = parse("int f() { return 0; }\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("int"));
}

#[test]
fn return_type_void() {
    let nodes = parse("void f() {}\n");
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("void"));
}

#[test]
fn struct_field() {
    let nodes = parse("struct S { int x; };\n");
    let x = find(&nodes, "x", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn struct_field_pointer() {
    let nodes = parse("struct S { char* name; };\n");
    let n = find(&nodes, "name", NodeKind::Property);
    assert_eq!(n.type_annotation.as_deref(), Some("char*"));
}

#[test]
fn var_with_qualifiers() {
    // Documented convention: the full declaration prefix is kept, so
    // `static const int N = 5;` → `"static const int"`. Consumers wanting
    // only the bare type can strip storage-class words downstream.
    // `const` on the object makes `N` a Const (FU 9adc33b1cf3e).
    let nodes = parse("static const int N = 5;\n");
    let v = find(&nodes, "N", NodeKind::Const);
    let annot = v.type_annotation.as_deref().unwrap_or("");
    assert!(
        annot.contains("int"),
        "expected `int` in annotation, got {annot:?}"
    );
    assert!(
        annot.contains("static") && annot.contains("const"),
        "expected qualifier prefix preserved, got {annot:?}"
    );
}

/// Contract: `Self make(void)` where the project declares a type named `Self` binds that
/// type. `Self` is no declaring-type keyword in C; a language-blind gate
/// would find no owner and bind nothing.
#[test]
fn test_self_named_project_type_binds_that_type() {
    let g = name_ref_support::graph(
        &CProvider::new().unwrap(),
        &[(
            "app.c",
            "typedef struct { int n; } Self;\n\nSelf make(void) { Self s = {0}; return s; }\n",
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
    assert_eq!(edges, vec![("app.c".to_string(), "Self".to_string())]);
}

/// Guard: a pointer return `Tok *` and `const Tok *` bind the project type
/// `Tok`, before and after every language reduced its annotation key: the C
/// parser already hands the resolver the bare type name. With no project
/// `Tok` nothing binds. C has no generics.
#[test]
fn test_pointer_return_annotation_binds_project_type() {
    let g = name_ref_support::graph(
        &CProvider::new().unwrap(),
        &[(
            "app.c",
            "typedef struct { int n; } Tok;\n\nTok *find(void) { return 0; }\n\nconst Tok *peek(void) { return 0; }\n",
        )],
    );
    let tok = [("app.c".to_string(), "Tok".to_string())];
    assert_eq!(annotated(&g, "find"), tok);
    assert_eq!(annotated(&g, "peek"), tok);

    let g = name_ref_support::graph(
        &CProvider::new().unwrap(),
        &[("app.c", "Tok *find(void) { return 0; }\n")],
    );
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}

/// A10 no-regression. Stands for: C has no class type parameters, so a field
/// typed with a project struct keeps binding it (positive only).
#[test]
fn test_field_annotation_no_class_type_parameters_binds_project_class() {
    let g = name_ref_support::graph(
        &CProvider::new().unwrap(),
        &[(
            "src/box.c",
            "struct Token { int x; };\nstruct Box { struct Token *tokens; };\n",
        )],
    );
    assert!(
        annotated(&g, "tokens").contains(&("src/box.c".to_string(), "Token".to_string())),
        "{:?}",
        annotated(&g, "tokens")
    );
}
