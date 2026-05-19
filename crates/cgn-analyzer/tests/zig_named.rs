//! Zig "Named" dimension: `const X = SomeType` emits NodeKind::Typedef when
//! the RHS is a bare identifier or a field_expression (qualified path).
//!
//! Plain value constants (`const x = 42`, `const s = "str"`, `const b = true`)
//! must NOT become Typedef — they stay NodeKind::Const.

use cgn_analyzer::zig::parser::ZigProvider;
use cgn_core::analyzer::provider::LanguageProvider;
use cgn_core::analyzer::types::LocalGraph;
use cgn_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    ZigProvider::new()
        .expect("provider")
        .parse_file(Path::new("test.zig"), src.as_bytes())
        .expect("parse")
}

fn kind_of(g: &LocalGraph, name: &str) -> Option<NodeKind> {
    g.nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn const_identifier_rhs_emits_typedef() {
    // const Allocator = std.mem.Allocator — actually field_expression; test bare identifier:
    // const T = SomeType;
    let g = parse("const T = SomeType;");
    assert_eq!(
        kind_of(&g, "T"),
        Some(NodeKind::Typedef),
        "`const T = SomeType` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn const_field_expression_rhs_emits_typedef() {
    // const Allocator = std.mem.Allocator;
    let g = parse("const Allocator = std.mem.Allocator;");
    assert_eq!(
        kind_of(&g, "Allocator"),
        Some(NodeKind::Typedef),
        "`const Allocator = std.mem.Allocator` must be NodeKind::Typedef; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn plain_integer_literal_not_typedef() {
    // const MAX: usize = 1024;  →  must be Const, not Typedef
    let g = parse("const MAX: usize = 1024;");
    assert_eq!(
        kind_of(&g, "MAX"),
        Some(NodeKind::Const),
        "`const MAX = 1024` must remain NodeKind::Const; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn plain_string_literal_not_typedef() {
    let g = parse(r#"const NAME = "hello";"#);
    assert_eq!(
        kind_of(&g, "NAME"),
        Some(NodeKind::Const),
        "`const NAME = \"hello\"` must remain NodeKind::Const; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn plain_bool_literal_not_typedef() {
    let g = parse("const FLAG = true;");
    assert_eq!(
        kind_of(&g, "FLAG"),
        Some(NodeKind::Const),
        "`const FLAG = true` must remain NodeKind::Const; nodes: {:#?}",
        g.nodes
    );
}

#[test]
fn typedef_and_const_coexist() {
    let g = parse(
        "const Allocator = std.mem.Allocator;\nconst MAX: usize = 1024;\nconst Handler = Handler;",
    );
    assert_eq!(kind_of(&g, "Allocator"), Some(NodeKind::Typedef));
    assert_eq!(kind_of(&g, "MAX"), Some(NodeKind::Const));
    assert_eq!(kind_of(&g, "Handler"), Some(NodeKind::Typedef));
}
