//! OpenAPI end-to-end graph emission: `OpenApiProvider::parse_file` →
//! `GraphBuilder::build()` → final `ZeroCopyGraph`.
//!
//! Regression for the dead-feature gap where OpenAPI schema output never
//! reached the graph: `components.schemas.<Name>` fields were dropped
//! because the schema emitted no owner node (`schema_field_mirrors`
//! couldn't resolve `owner_class` via the SymbolTable). End-to-end an
//! OpenAPI spec indexed to essentially no `SchemaField` nodes. This locks
//! in that a schema with ≥1 property now produces a `Struct` + its
//! `SchemaField`s (+ `HasProperty`).
//!
//! All existing openapi tests verify `parse_file` in isolation; this is the
//! only one that drives the full builder pipeline.

use ecp_analyzer::openapi::OpenApiProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build_openapi(src: &str) -> ZeroCopyGraph {
    let provider = OpenApiProvider::new().expect("provider");
    let lg = provider
        .parse_file(Path::new("api.yaml"), src.as_bytes())
        .expect("parse");
    let mut builder = GraphBuilder::new();
    builder.add_graph(lg);
    builder.build()
}

fn names_of_kind(graph: &ZeroCopyGraph, kind: NodeKind) -> Vec<String> {
    let pool = graph.string_pool.as_slice();
    graph
        .nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.resolve(pool).to_string())
        .collect()
}

#[test]
fn schema_reaches_graph_as_struct_with_schema_fields() {
    let spec = "\
openapi: 3.0.0
info:
  title: Test API
  version: 1.0.0
paths: {}
components:
  schemas:
    User:
      type: object
      properties:
        email:
          type: string
        age:
          type: integer
";
    let graph = build_openapi(spec);

    assert_eq!(
        names_of_kind(&graph, NodeKind::Struct),
        vec!["User".to_string()],
        "schema → Struct node"
    );
    let mut fields = names_of_kind(&graph, NodeKind::SchemaField);
    fields.sort();
    assert_eq!(fields, vec!["age".to_string(), "email".to_string()]);

    // HasProperty: Struct → SchemaField for each field.
    let has_property = graph
        .edges
        .iter()
        .filter(|e| {
            e.rel_type == RelType::HasProperty
                && graph.nodes[e.source as usize].kind == NodeKind::Struct
                && graph.nodes[e.target as usize].kind == NodeKind::SchemaField
        })
        .count();
    assert_eq!(has_property, 2, "User owns both fields via HasProperty");
}

#[test]
fn empty_schema_emits_no_struct_node() {
    // A schema with no properties has no schema surface to own — emitting a
    // node would leave an orphan with no HasProperty edge.
    let spec = "\
openapi: 3.0.0
info:
  title: Test API
  version: 1.0.0
paths: {}
components:
  schemas:
    Empty:
      type: object
";
    let graph = build_openapi(spec);
    assert!(names_of_kind(&graph, NodeKind::Struct).is_empty());
    assert!(names_of_kind(&graph, NodeKind::SchemaField).is_empty());
}

#[test]
fn populated_and_empty_schemas_coexist_in_graph() {
    let spec = "\
openapi: 3.0.0
info:
  title: Test API
  version: 1.0.0
paths: {}
components:
  schemas:
    Req:
      type: object
      properties:
        id:
          type: string
    Empty:
      type: object
";
    let graph = build_openapi(spec);
    assert_eq!(
        names_of_kind(&graph, NodeKind::Struct),
        vec!["Req".to_string()]
    );
    assert_eq!(
        names_of_kind(&graph, NodeKind::SchemaField),
        vec!["id".to_string()]
    );
}
