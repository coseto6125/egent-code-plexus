//! Regression tests for C# symbol-emission gaps identified in
//! `scripts/parity/final_baseline.txt`:
//!
//! - `Namespace` was 0 emits despite parser.rs having `idx_namespace` and
//!   queries.scm having a capture pattern — dead path.
//! - `Enum` was 0 emits despite identical (working) class/interface pattern.
//! - `Struct` was being emitted as `Class` (queries.scm captured @name.class
//!   for struct_declaration); ref-gitnexus emits 29.
//! - `Annotation` (custom-attribute classes) had no detection path at all.

use ecp_analyzer::c_sharp::parser::CSharpProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = CSharpProvider::new().expect("provider");
    p.parse_file(Path::new("test.cs"), src.as_bytes())
        .expect("parse")
}

fn names_of_kind(g: &LocalGraph, k: NodeKind) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == k)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn block_namespace_emits_namespace_node() {
    let g = parse("namespace Foo { class Bar { } }");
    let ns = names_of_kind(&g, NodeKind::Namespace);
    assert_eq!(ns, vec!["Foo"], "nodes: {:?}", g.nodes);
}

#[test]
fn file_scoped_namespace_emits_namespace_node() {
    let g = parse("namespace Foo.Bar;\nclass Baz { }");
    let ns = names_of_kind(&g, NodeKind::Namespace);
    assert_eq!(ns.len(), 1, "nodes: {:?}", g.nodes);
    assert!(
        ns[0].contains("Foo"),
        "expected qualified Foo.Bar, got {}",
        ns[0]
    );
}

#[test]
fn plain_enum_emits_enum_node() {
    let g = parse("enum Color { Red, Green, Blue }");
    let enums = names_of_kind(&g, NodeKind::Enum);
    assert_eq!(enums, vec!["Color"], "nodes: {:?}", g.nodes);
}

#[test]
fn typed_enum_emits_enum_node() {
    let g = parse("enum Status : int { Active = 1, Idle = 2 }");
    let enums = names_of_kind(&g, NodeKind::Enum);
    assert_eq!(enums, vec!["Status"], "nodes: {:?}", g.nodes);
}

#[test]
fn struct_emits_struct_node_not_class() {
    let g = parse("struct Point { public int X; public int Y; }");
    let structs = names_of_kind(&g, NodeKind::Struct);
    let classes = names_of_kind(&g, NodeKind::Class);
    assert_eq!(structs, vec!["Point"], "nodes: {:?}", g.nodes);
    assert!(
        !classes.contains(&"Point"),
        "struct should not also emit Class: {:?}",
        g.nodes
    );
}

#[test]
fn attribute_class_emits_annotation_kind() {
    let src = r#"
        using System;
        public class MyAttribute : Attribute {
            public string Name { get; set; }
        }
    "#;
    let g = parse(src);
    let annos = names_of_kind(&g, NodeKind::Annotation);
    let classes = names_of_kind(&g, NodeKind::Class);
    assert_eq!(annos, vec!["MyAttribute"], "nodes: {:?}", g.nodes);
    assert!(
        !classes.contains(&"MyAttribute"),
        "attribute class should not double-emit as Class: {:?}",
        g.nodes
    );
}

#[test]
fn attribute_class_with_preprocessor_recovery_keeps_real_name() {
    let src = r#"
public class MyAttribute
#if NET8_0_OR_GREATER
    : Attribute
#endif
{
}
"#;
    let g = parse(src);
    let annos = names_of_kind(&g, NodeKind::Annotation);
    assert_eq!(annos, vec!["MyAttribute"], "nodes: {:?}", g.nodes);
}

#[test]
fn mixed_declarations_all_emit_correct_kinds() {
    let src = r#"
        namespace App {
            struct Vec3 { }
            enum Color { Red }
            class FooAttribute : Attribute { }
            class Bar { }
        }
    "#;
    let g = parse(src);
    let ns = names_of_kind(&g, NodeKind::Namespace);
    let st = names_of_kind(&g, NodeKind::Struct);
    let en = names_of_kind(&g, NodeKind::Enum);
    let an = names_of_kind(&g, NodeKind::Annotation);
    let cl = names_of_kind(&g, NodeKind::Class);
    assert_eq!(ns, vec!["App"], "namespace");
    assert_eq!(st, vec!["Vec3"], "struct");
    assert_eq!(en, vec!["Color"], "enum");
    assert_eq!(an, vec!["FooAttribute"], "annotation");
    assert_eq!(cl, vec!["Bar"], "plain class");
}
