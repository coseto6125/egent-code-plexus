//! Go `type_annotation` capture coverage (Wave 2 / Task D1).
//!
//! Pins that the Go provider now populates `RawNode.type_annotation` for:
//!   * function / method parameter names (Variable nodes)
//!   * struct field names (Property nodes)
//!   * function / method return types (Function / Method nodes)
//!   * top-level `var` declarations with an explicit type (Variable nodes)
//!
//! Short declarations (`n := 1`) intentionally leave `type_annotation=None`
//! because Go's grammar exposes no `type:` field there — `ecp context` should
//! reflect that the type is inferred, not made up.
//!
//! Spec: `docs/specs/2026-05-15-language-coverage-gaps.md` Wave 2 / D1.

use crate::name_ref_support;

use ecp_analyzer::go::parser::GoProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use name_ref_support::annotated;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = GoProvider::new().expect("GoProvider init");
    let graph = provider
        .parse_file(Path::new("test.go"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

#[test]
fn test_parse_file_import_name_forms_preserve_unique_binding() {
    let provider = GoProvider::new().expect("GoProvider init");
    for (binding, alias, imported_name) in [
        ("", None, "client"),
        ("local ", Some("local"), "local"),
        (". ", Some("."), "client"),
        ("_ ", Some("_"), "client"),
    ] {
        let source = format!("package app\nimport {binding}\"github.com/ext/client\"\n");
        let graph = provider
            .parse_file(Path::new("imports.go"), source.as_bytes())
            .expect("parse_file");
        assert_eq!(graph.imports.len(), 1, "{source}");
        assert_eq!(graph.imports[0].source, "github.com/ext/client");
        assert_eq!(graph.imports[0].alias.as_deref(), alias);
        assert_eq!(graph.imports[0].imported_name, imported_name);
    }
}

// param_type_* tests removed: formal parameters are no longer emitted as
// Variable nodes (see `fix(analyzer): drop formal_parameter Variable
// emission ...`).

#[test]
fn field_type_basic() {
    let src = "package p\ntype S struct {\n  X int\n}\n";
    let nodes = parse(src);
    let x = find(&nodes, "X", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn field_type_slice() {
    let src = "package p\ntype S struct {\n  Tags []string\n}\n";
    let nodes = parse(src);
    let tags = find(&nodes, "Tags", NodeKind::Property);
    assert_eq!(tags.type_annotation.as_deref(), Some("[]string"));
}

#[test]
fn return_type_single() {
    let src = "package p\nfunc f() int { return 0 }\n";
    let nodes = parse(src);
    let f = find(&nodes, "f", NodeKind::Function);
    assert_eq!(f.type_annotation.as_deref(), Some("int"));
}

#[test]
fn return_type_multi() {
    let src = "package p\nfunc f() (int, error) { return 0, nil }\n";
    let nodes = parse(src);
    let f = find(&nodes, "f", NodeKind::Function);
    // The grammar captures the whole `(int, error)` parameter_list span.
    assert_eq!(f.type_annotation.as_deref(), Some("(int, error)"));
}

#[test]
fn var_declaration_explicit() {
    let src = "package p\nvar n int = 1\n";
    let nodes = parse(src);
    let n = find(&nodes, "n", NodeKind::Variable);
    assert_eq!(n.type_annotation.as_deref(), Some("int"));
}

#[test]
fn var_declaration_inferred_no_annotation() {
    // Short declarations have no `type:` field — provider emits Variable
    // with type_annotation=None (parity with ref gitnexus oracle).
    let src = "package p\nfunc f() { n := 1; _ = n }\n";
    let nodes = parse(src);
    let n_var = nodes
        .iter()
        .find(|n| n.name == "n" && n.kind == NodeKind::Variable)
        .expect("short-decl `n := 1` must emit a Variable node");
    assert!(
        n_var.type_annotation.is_none(),
        "short-decl Variable must have type_annotation=None; got {:?}",
        n_var.type_annotation,
    );
}

// ─── Multi-name regression (PR #2 review issue #1) ────────────────────────
//
// Pre-fix: `param_name_node` / `var_name_node` were `Option<Node>` and got
// overwritten by each capture in a multi-name match, so only the LAST name
// emitted a Variable node. These tests pin the contract that EVERY name in
// a multi-name decl produces its own Variable carrying the shared type.

// multi_name_param_emits_one_variable_per_name removed (params no longer
// emit Variable nodes); multi_name_var_decl_emits_one_variable_per_name
// kept — that case (`var X, Y int`) still emits two Variables.

#[test]
fn multi_name_var_decl_emits_one_variable_per_name() {
    let src = "package p\nvar X, Y int\n";
    let nodes = parse(src);
    let x = find(&nodes, "X", NodeKind::Variable);
    let y = find(&nodes, "Y", NodeKind::Variable);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
    assert_eq!(y.type_annotation.as_deref(), Some("int"));
    // Go uppercase-first → exported.
    assert!(x.is_exported);
    assert!(y.is_exported);
}

/// Contract: `func Make() Self` where the project declares a type named `Self` binds that
/// type. `Self` is no declaring-type keyword in Go; a language-blind gate
/// would find no owner and bind nothing.
#[test]
fn test_self_named_project_type_binds_that_type() {
    let g = name_ref_support::graph(
        &GoProvider::new().unwrap(),
        &[(
            "app.go",
            "package app\n\ntype Self struct{ n int }\n\nfunc Make() Self { return Self{} }\n",
        )],
    );
    let edges: Vec<(String, String)> = name_ref_support::refs(
        &g,
        "Make",
        ecp_core::graph::RelType::Accesses,
        "type_annotation",
    )
    .into_iter()
    .map(|(file, name, _, _)| (file, name))
    .collect();
    assert_eq!(edges, vec![("app.go".to_string(), "Self".to_string())]);
}

/// The Go query captures the `type_identifier` inside a `pointer_type`, so
/// `*Tok` reaches the edge as `Tok`. `[]Tok` is a `slice_type` the query does
/// not capture; its text comes from the function meta, and it binds `Tok` as
/// the element type.
#[test]
fn test_pointer_and_slice_returns_bind_project_type() {
    let g = name_ref_support::graph(
        &GoProvider::new().unwrap(),
        &[(
            "app.go",
            "package app\n\ntype Tok struct{ n int }\n\nfunc Find() *Tok { return nil }\n\nfunc All() []Tok { return nil }\n",
        )],
    );
    assert_eq!(
        annotated(&g, "Find"),
        [("app.go".to_string(), "Tok".to_string())]
    );
    assert_eq!(
        annotated(&g, "All"),
        [("app.go".to_string(), "Tok".to_string())]
    );
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::go::parser::GoProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(_token: &str) -> String {
        "main.go".to_string()
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let params = generic.map_or(String::new(), |p| format!("[{p} any]"));
        let files = [(
            token_file(token),
            format!(
                "package main\n\ntype {token} struct{{}}\ntype List[E any] struct{{}}\ntype Box{params} struct {{\n\titems List[{arg}]\n}}\ntype Holder struct {{\n\ttokens List[{token}]\n}}\n"
            ),
        )];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&GoProvider::new().unwrap(), &refs)
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
        vec![(
            token_file("Token"),
            "package main\n\ntype Token struct{}\ntype List[E any] struct{}\nfunc put[Token any](xs List[Token]) {}\nfunc take(xs List[Token]) {}\n".to_string(),
        )]
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
        let g = graph(&GoProvider::new().unwrap(), &refs);
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
