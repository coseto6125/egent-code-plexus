//! Per-framework SchemaFieldConfig constants for protobuf.
//!
//! Protobuf field extraction does NOT use the tree-sitter `extract_schema_fields`
//! dispatcher because there is no `tree-sitter-protobuf` dependency.  The
//! `PROTOBUF_FIELD_MODIFIERS` and `PROTOBUF_SCALAR_TYPES` constants below are
//! consumed directly by the hand-rolled lexer in `parser.rs`.
//!
//! T4-5 (this module): `.proto` message field → `RawSchemaField`.

use ecp_core::analyzer::types::{FrameworkId, SchemaType};

/// Framework identity written into every `RawSchemaField` emitted by the
/// protobuf lexer.
pub const PROTOBUF_FRAMEWORK: FrameworkId = FrameworkId::Protobuf;

/// Field modifiers that may appear before the type token in proto2/proto3.
/// The lexer strips any leading token that matches one of these before
/// attempting to parse `<type> <name> = <number>`.
pub const PROTOBUF_FIELD_MODIFIERS: &[&str] = &[
    "optional",
    "required",
    "repeated",
    "singular",
    "proto3optional",
];

/// Canonical type classifier for protobuf scalar types.
///
/// Maps the proto scalar type tokens defined in the proto2/proto3 language
/// guide to the shared `SchemaType` enum.  Custom message-type references
/// (e.g. `google.protobuf.Timestamp`, `MyMessage`) fall through to
/// `SchemaType::Other`.
pub fn classify_protobuf_type(raw: &str) -> SchemaType {
    match raw {
        "string" | "bytes" => SchemaType::String,
        "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" | "fixed32" | "fixed64"
        | "sfixed32" | "sfixed64" => SchemaType::Int,
        "float" | "double" => SchemaType::Float,
        "bool" => SchemaType::Bool,
        // All other tokens (custom message types, enums, google.protobuf.* wkt)
        // are treated as opaque references.
        _ => SchemaType::Other,
    }
}
