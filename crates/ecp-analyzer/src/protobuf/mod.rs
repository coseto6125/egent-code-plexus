//! Protobuf `.proto` file analysis — message structs, schema fields, and
//! gRPC service contracts.
//!
//! Uses a hand-rolled line-oriented lexer (Option B) because no
//! `tree-sitter-protobuf` crate exists in the workspace.  The lexer handles
//! the proto2/proto3 field subset needed for schema-field extraction:
//!
//! ```proto
//! message Foo {
//!     optional string name = 1;
//!     repeated int32  ids  = 2;
//! }
//! ```
//!
//! Each top-level `message` with ≥1 field becomes a `NodeKind::Struct`
//! (value-type aggregate — no inheritance/vtable, distinct from `Class`),
//! owning its fields via `HasProperty`. Without this owner node the schema
//! fields are dropped at `schema_field_mirrors` and never reach the graph.
//!
//! **Acknowledged limitations (v1)**:
//! - Nested `message` definitions are skipped (parser does not recurse).
//! - `oneof` blocks are not supported — fields inside them are not emitted.
//! - `map<K,V>` fields are not supported — skipped with no emission.
//! - `enum` definitions are ignored (no `SchemaField` equivalent).
//! - `service { rpc … }` blocks ARE captured: each `rpc` becomes a
//!   `NodeKind::Route` (method `GRPC`, path `/<package.>Service/Method`) so
//!   gRPC service contracts are visible to `ecp routes` / `ecp contracts`.
//!   Nested services and rpc request/response message types are not captured.
//! - Multi-line comments (`/* … */`) are treated as opaque — a field
//!   declaration whose line falls inside a block comment may be emitted.
//!   Single-line `//` comments are stripped correctly.
//! - Options (`[deprecated = true]`) are tolerated: the field number / option
//!   tail is dropped before the name+type are extracted.

pub mod parser;
pub mod schema_extractors;

pub use parser::ProtobufProvider;
