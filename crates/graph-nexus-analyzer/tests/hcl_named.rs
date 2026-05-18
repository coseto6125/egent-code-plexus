//! HCL Named dimension: `locals { ... }` attribute keys emit `NodeKind::Typedef`.
//!
//! Each key in a `locals` block is a named alias for a computed expression.
//! Existing `output`, `variable`, `resource`, and `data` captures are unchanged.

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

fn find_kind<'a>(nodes: &'a [RawNode], name: &str, kind: NodeKind) -> &'a RawNode {
    nodes
        .iter()
        .find(|n| n.name == name && n.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} node `{name}` in {nodes:#?}"))
}

#[test]
fn test_hcl_locals_attr_emits_typedef() {
    let src = r#"
locals {
  region = "us-east-1"
  env    = "prod"
}
"#;
    let nodes = parse(src);
    find_kind(&nodes, "region", NodeKind::Typedef);
    find_kind(&nodes, "env", NodeKind::Typedef);
}

#[test]
fn test_hcl_attr_outside_locals_not_typedef() {
    // A `variable` block attribute must NOT appear as Typedef.
    let src = r#"
variable "bucket_name" {
  default = "my-bucket"
}

locals {
  prefix = "dev"
}
"#;
    let nodes = parse(src);
    // `bucket_name` emits as Const (variable block), never as Typedef.
    assert!(
        nodes
            .iter()
            .find(|n| n.name == "bucket_name" && n.kind == NodeKind::Typedef)
            .is_none(),
        "variable block attribute must not emit as Typedef; nodes: {nodes:#?}"
    );
    // `prefix` in locals is Typedef.
    find_kind(&nodes, "prefix", NodeKind::Typedef);
}

#[test]
fn test_hcl_output_block_still_emits_const() {
    // `output` blocks must remain NodeKind::Const (module public interface).
    let src = r#"
output "vpc_id" {
  value = "vpc-abc"
}

locals {
  tag = "v1"
}
"#;
    let nodes = parse(src);
    find_kind(&nodes, "vpc_id", NodeKind::Const);
    find_kind(&nodes, "tag", NodeKind::Typedef);
}

#[test]
fn test_hcl_multiple_locals_attrs() {
    let src = r#"
locals {
  alpha = 1
  beta  = 2
  gamma = 3
}
"#;
    let nodes = parse(src);
    find_kind(&nodes, "alpha", NodeKind::Typedef);
    find_kind(&nodes, "beta", NodeKind::Typedef);
    find_kind(&nodes, "gamma", NodeKind::Typedef);
}
