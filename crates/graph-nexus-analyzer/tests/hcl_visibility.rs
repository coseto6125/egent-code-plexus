//! Visibility checks for the HCL/Terraform provider.
//!
//! Terraform `output "name" { ... }` blocks declare a module's public interface
//! and must be emitted with `is_exported = true`.  All other block types
//! (`variable`, `resource`, `data`, `locals`) are internal and must be emitted
//! with `is_exported = false`.

use graph_nexus_analyzer::hcl::parser::HclProvider;
use graph_nexus_core::analyzer::provider::LanguageProvider;
use graph_nexus_core::analyzer::types::RawNode;
use graph_nexus_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> Vec<RawNode> {
    let provider = HclProvider::new().expect("HclProvider init");
    let graph = provider
        .parse_file(Path::new("main.tf"), src.as_bytes())
        .expect("parse_file");
    graph.nodes
}

fn find<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn output_block_is_exported() {
    let src = r#"
output "public" {
  value = aws_instance.main.id
}
"#;
    let nodes = parse(src);
    let node = find(&nodes, "public", NodeKind::Const);
    assert!(node.is_exported, "`output \"public\"` must be exported");
}

#[test]
fn variable_block_is_not_exported() {
    let src = r#"
variable "private" {
  type = string
}
"#;
    let nodes = parse(src);
    let node = find(&nodes, "private", NodeKind::Const);
    assert!(!node.is_exported, "`variable \"private\"` must NOT be exported");
}

#[test]
fn output_and_variable_together() {
    let src = r#"
output "bucket_name" {
  value = aws_s3_bucket.main.bucket
}

variable "region" {
  type    = string
  default = "us-east-1"
}
"#;
    let nodes = parse(src);

    let out = find(&nodes, "bucket_name", NodeKind::Const);
    assert!(out.is_exported, "`output \"bucket_name\"` must be exported");

    let var = find(&nodes, "region", NodeKind::Const);
    assert!(!var.is_exported, "`variable \"region\"` must NOT be exported");
}
