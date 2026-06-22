//! Protobuf end-to-end graph emission: `ProtobufProvider::parse_file` →
//! `GraphBuilder::build()` → final `ZeroCopyGraph`.
//!
//! Regression for the dead-feature gap where proto output never reached the
//! graph: `message` fields were dropped because the message emitted no owner
//! node (`schema_field_mirrors` couldn't resolve the owner class), and gRPC
//! routes were dropped by the HTTP-only `detect_from_call`. End-to-end a
//! `.proto` indexed to a lone File node. This locks in that a message now
//! produces a `Struct` + its `SchemaField`s (+ `HasProperty`) and a service
//! produces `Route`s.
//!
//! All existing proto tests verify `parse_file` in isolation; this is the
//! only one that drives the full builder pipeline.

use ecp_analyzer::protobuf::ProtobufProvider;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
use std::path::Path;

fn build_proto(src: &str) -> ZeroCopyGraph {
    let provider = ProtobufProvider::new().expect("provider");
    let lg = provider
        .parse_file(Path::new("api.proto"), src.as_bytes())
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
fn message_reaches_graph_as_struct_with_schema_fields() {
    let proto = "\
syntax = \"proto3\";
package api.v1;

message User {
  string email = 1;
  int32 age = 2;
}
";
    let graph = build_proto(proto);

    assert_eq!(
        names_of_kind(&graph, NodeKind::Struct),
        vec!["User".to_string()],
        "message → Struct node"
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
fn empty_message_emits_no_struct_node() {
    // A message with no fields has no schema surface to own — emitting a node
    // would leave an orphan with no HasProperty edge.
    let proto = "package m;\nmessage Empty {\n}\n";
    let graph = build_proto(proto);
    assert!(names_of_kind(&graph, NodeKind::Struct).is_empty());
}

#[test]
fn service_and_messages_coexist_in_graph() {
    let proto = "\
package api;

message Req {
  string id = 1;
}

service Svc {
  rpc Do (Req) returns (Req);
}
";
    let graph = build_proto(proto);
    assert_eq!(
        names_of_kind(&graph, NodeKind::Struct),
        vec!["Req".to_string()]
    );
    assert_eq!(
        names_of_kind(&graph, NodeKind::Route),
        vec!["GRPC /api.Svc/Do".to_string()]
    );
}
