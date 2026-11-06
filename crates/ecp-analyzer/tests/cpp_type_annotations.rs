//! Type annotations on C++ nodes (params, fields, return types, vars, methods).
//!
//! Covers Wave 2 task D3 from
//! `docs/specs/2026-05-15-language-coverage-gaps.md`.
//!
//! Conventions documented in `src/cpp/parser.rs::slice_type_before`:
//! - **Pointer / reference spacing**: preserved as-written. `char* s` →
//!   `"char*"`, `const std::string& s` → `"const std::string&"`.
//! - **Qualifier inclusion**: full prefix kept (storage-class, cv).
//! - **Templates**: preserved verbatim (`std::vector<int>`,
//!   `std::map<std::string,User>`).
//! - **`auto`**: kept literally — the analyzer does not perform deduction.

use crate::name_ref_support;

use ecp_analyzer::cpp::parser::CppProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::RawNode;
use ecp_core::graph::NodeKind;
use name_ref_support::annotated;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = CppProvider::new().expect("CppProvider init");
    let graph = provider
        .parse_file(Path::new("t.cpp"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} `{name}` in {nodes:#?}"))
}

// param_template / param_reference removed: formal parameters are no
// longer emitted as Variable nodes (see `fix(analyzer): drop
// formal_parameter Variable emission ...`).

#[test]
fn return_type_template() {
    let nodes = parse("std::map<std::string,User> getAll();\n");
    let f = find(&nodes, "getAll", NodeKind::Function);
    assert_eq!(
        f.type_annotation.as_deref(),
        Some("std::map<std::string,User>")
    );
}

#[test]
fn class_field() {
    let nodes = parse("class C { int x; };\n");
    let x = find(&nodes, "x", NodeKind::Property);
    assert_eq!(x.type_annotation.as_deref(), Some("int"));
}

#[test]
fn auto_var() {
    // Documented choice: keep `auto` literal. Deducing the actual type from
    // the initializer requires semantic analysis the analyzer doesn't perform.
    let nodes = parse("auto x = 5;\n");
    let v = find(&nodes, "x", NodeKind::Variable);
    assert_eq!(v.type_annotation.as_deref(), Some("auto"));
}

#[test]
fn member_function_return() {
    // Member function declared inside a class body — `class C { int sum(); };`
    // — must emit a Method node carrying the return-type annotation.
    let nodes = parse("class C { int sum(); };\n");
    let m = find(&nodes, "sum", NodeKind::Method);
    assert_eq!(m.type_annotation.as_deref(), Some("int"));
}

// param_primitive removed (see above).

#[test]
fn class_field_template() {
    let nodes = parse("class C { std::vector<int> items; };\n");
    let p = find(&nodes, "items", NodeKind::Property);
    assert_eq!(p.type_annotation.as_deref(), Some("std::vector<int>"));
}

/// Guard: pointer and reference returns (`Tok *`, `const Tok &`) bind the
/// project struct `Tok`, before and after every language reduced its
/// annotation key: the C++ parser already hands the resolver the bare type
/// name. With no project `Tok` nothing binds.
#[test]
fn test_pointer_and_reference_return_annotations_bind_project_type() {
    let g = name_ref_support::graph(
        &CppProvider::new().unwrap(),
        &[(
            "app.cpp",
            "struct Tok {};\n\nTok *find() { return nullptr; }\n\nconst Tok &peek(const Tok &t) { return t; }\n",
        )],
    );
    let tok = [("app.cpp".to_string(), "Tok".to_string())];
    assert_eq!(annotated(&g, "find"), tok);
    assert_eq!(annotated(&g, "peek"), tok);

    let g = name_ref_support::graph(
        &CppProvider::new().unwrap(),
        &[("app.cpp", "Tok *find() { return nullptr; }\n")],
    );
    assert_eq!(annotated(&g, "find"), Vec::<(String, String)>::new());
}

/// `std::vector<Tok>` names `Tok` as its element type, so the return binds it.
#[test]
fn test_generic_return_annotation_binds_the_element_type() {
    let g = name_ref_support::graph(
        &CppProvider::new().unwrap(),
        &[(
            "app.cpp",
            "struct Tok {};\n\nstd::vector<Tok> all() { return {}; }\n",
        )],
    );
    assert_eq!(
        annotated(&g, "all"),
        [("app.cpp".to_string(), "Tok".to_string())]
    );
}

/// A10: a field type argument that names a class type parameter binds no
/// project type, even when a project class shares the parameter's name.
mod a10 {
    use crate::name_ref_support::{annotated, graph};
    use ecp_analyzer::cpp::parser::CppProvider;
    use ecp_core::graph::ZeroCopyGraph;

    fn pair(file: &str, name: &str) -> (String, String) {
        (file.to_string(), name.to_string())
    }

    /// Target names of every type-annotation edge out of `source`.
    fn names(g: &ZeroCopyGraph, source: &str) -> Vec<String> {
        annotated(g, source).into_iter().map(|(_, n)| n).collect()
    }

    fn token_file(_token: &str) -> String {
        "tokens.hpp".to_string()
    }

    fn build(generic: Option<&str>, token: &str) -> ZeroCopyGraph {
        let arg = generic.unwrap_or(token);
        let params = generic.map_or(String::new(), |p| format!("<{p}>"));
        let _ = (&arg, &params);
        let template = generic.map_or(String::new(), |p| format!("template <class {p}>\n"));
        let files = [(
            token_file(token),
            format!(
                "#include <vector>\nstruct {token} {{}};\n{template}struct Box {{\n    std::vector<{arg}> items;\n}};\nstruct Holder {{\n    std::vector<{token}> tokens;\n}};\n"
            ),
        )];
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        graph(&CppProvider::new().unwrap(), &refs)
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
}
