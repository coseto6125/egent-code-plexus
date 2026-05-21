//! Protobuf `.proto` file analysis — T4-5 schema-field detector.
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
//! **Acknowledged limitations (v1)**:
//! - Nested `message` definitions are skipped (parser does not recurse).
//! - `oneof` blocks are not supported — fields inside them are not emitted.
//! - `map<K,V>` fields are not supported — skipped with no emission.
//! - `enum` definitions are ignored (no `SchemaField` equivalent).
//! - RPC / service blocks are ignored.
//! - Multi-line comments (`/* … */`) are treated as opaque — a field
//!   declaration whose line falls inside a block comment may be emitted.
//!   Single-line `//` comments are stripped correctly.
//! - Options (`[deprecated = true]`) are tolerated: the field number / option
//!   tail is dropped before the name+type are extracted.

pub mod parser;
pub mod schema_extractors;

pub use parser::ProtobufProvider;
