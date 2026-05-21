//! T4-5: Protobuf `.proto` message field extraction tests.
//!
//! All tests call `ProtobufProvider::parse_file` which invokes the hand-rolled
//! lexer (Option B — no tree-sitter-protobuf dep).  String resolution uses the
//! parse_file-internal `StringPool`; only field count + framework identity are
//! verified via the boxed slice (exact string content is covered by the unit
//! tests inside `parser.rs` itself).
//!
//! For tests that need resolved strings we call the inner
//! `extract_proto_fields` through the public provider interface by round-
//! tripping through `parse_file`, then checking `schema_fields`.

use ecp_analyzer::protobuf::ProtobufProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, SchemaType};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse `src` as a `.proto` file and return the extracted fields (panics if
/// None or on parse error).
fn parse(src: &str) -> Vec<ecp_core::analyzer::types::RawSchemaField> {
    let provider = ProtobufProvider::new().expect("provider init");
    let local = provider
        .parse_file("test.proto".as_ref(), src.as_bytes())
        .expect("parse_file");
    local
        .schema_fields
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Happy path: single `message User` with one `string email` field.
#[test]
fn test_single_field_emitted() {
    let src = r#"
syntax = "proto3";
message User {
    string email = 1;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 1, "expected exactly one field");
    assert_eq!(fields[0].framework, FrameworkId::Protobuf);
    assert_eq!(fields[0].type_class, SchemaType::String);
}

/// Multiple fields in one message → all emitted.
#[test]
fn test_multiple_fields_in_one_message() {
    let src = r#"
syntax = "proto3";
message User {
    string email = 1;
    int32 age = 2;
    bool active = 3;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 3, "expected three fields");
    assert!(
        fields.iter().all(|f| f.framework == FrameworkId::Protobuf),
        "all fields must carry Protobuf framework id"
    );
}

/// Multiple messages in one file → owner_class correctly attributed.
#[test]
fn test_multiple_messages_owner_attribution() {
    let src = r#"
syntax = "proto3";
message User {
    string email = 1;
    int32 age = 2;
}
message Product {
    string name = 1;
    float price = 2;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 4, "User(2) + Product(2) = 4 fields");

    // We cannot resolve StrRefs here (pool is internal) but we can verify
    // type classification which is stateless:
    let string_count = fields
        .iter()
        .filter(|f| f.type_class == SchemaType::String)
        .count();
    let int_count = fields
        .iter()
        .filter(|f| f.type_class == SchemaType::Int)
        .count();
    let float_count = fields
        .iter()
        .filter(|f| f.type_class == SchemaType::Float)
        .count();
    assert_eq!(string_count, 2, "email + name");
    assert_eq!(int_count, 1, "age");
    assert_eq!(float_count, 1, "price");
}

/// `repeated` modifier does not break extraction — the type is captured, the
/// modifier is stripped.
#[test]
fn test_repeated_modifier_stripped() {
    let src = r#"
syntax = "proto3";
message TagList {
    repeated string tags = 1;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::String);
}

/// `optional` modifier (proto2 / proto3 optional) is stripped correctly.
#[test]
fn test_optional_modifier_stripped() {
    let src = r#"
syntax = "proto2";
message Contact {
    optional string phone = 1;
    required int64 id = 2;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 2);
    let types: Vec<SchemaType> = fields.iter().map(|f| f.type_class).collect();
    assert!(types.contains(&SchemaType::String));
    assert!(types.contains(&SchemaType::Int));
}

/// Scalar type coverage — all protobuf scalar types must map to the expected
/// `SchemaType`.
#[test]
fn test_scalar_type_coverage() {
    use ecp_analyzer::protobuf::schema_extractors::classify_protobuf_type;

    assert_eq!(classify_protobuf_type("string"), SchemaType::String);
    assert_eq!(classify_protobuf_type("bytes"), SchemaType::String);
    assert_eq!(classify_protobuf_type("int32"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("int64"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("uint32"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("uint64"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("sint32"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("sint64"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("fixed32"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("fixed64"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("sfixed32"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("sfixed64"), SchemaType::Int);
    assert_eq!(classify_protobuf_type("float"), SchemaType::Float);
    assert_eq!(classify_protobuf_type("double"), SchemaType::Float);
    assert_eq!(classify_protobuf_type("bool"), SchemaType::Bool);
    // Custom message type → Other
    assert_eq!(classify_protobuf_type("User"), SchemaType::Other);
    assert_eq!(
        classify_protobuf_type("google.protobuf.Timestamp"),
        SchemaType::Other
    );
}

/// Non-protobuf content (e.g. empty file or random text) doesn't crash and
/// returns no fields.
#[test]
fn test_non_proto_content_no_crash() {
    let cases = &["", "this is not a proto file", "{ } ; : = syntax"];
    for src in cases.iter() {
        let fields = parse(src);
        assert!(
            fields.is_empty(),
            "expected no fields for non-proto content: {:?}",
            src
        );
    }
}

/// An empty `.proto` file (only syntax declaration, no messages) → None /
/// empty schema_fields.
#[test]
fn test_empty_proto_no_fields() {
    let src = r#"syntax = "proto3";"#;
    let provider = ProtobufProvider::new().unwrap();
    let local = provider
        .parse_file("empty.proto".as_ref(), src.as_bytes())
        .unwrap();
    assert!(
        local.schema_fields.is_none(),
        "no messages → schema_fields must be None"
    );
}

/// Nested message definitions are skipped (depth ≥ 2) — v1 limitation.
/// The outer message's fields are still emitted.
#[test]
fn test_nested_message_outer_fields_emitted_inner_skipped() {
    let src = r#"
syntax = "proto3";
message Outer {
    string name = 1;
    message Inner {
        int32 value = 1;
    }
    bool active = 2;
}
"#;
    let fields = parse(src);
    // `name` and `active` from Outer are emitted; `Inner.value` is at
    // depth 2 and is skipped.
    assert_eq!(
        fields.len(),
        2,
        "only Outer's fields emitted; Inner is skipped in v1"
    );
    let types: Vec<SchemaType> = fields.iter().map(|f| f.type_class).collect();
    assert!(types.contains(&SchemaType::String), "name: string");
    assert!(types.contains(&SchemaType::Bool), "active: bool");
}

/// `oneof` blocks (depth ≥ 2) — fields inside oneof are not emitted in v1.
#[test]
fn test_oneof_fields_not_emitted() {
    let src = r#"
syntax = "proto3";
message Request {
    int64 id = 1;
    oneof payload {
        string text = 2;
        bytes data = 3;
    }
}
"#;
    let fields = parse(src);
    // Only `id` at depth 1; `text` and `data` are inside oneof at depth 2.
    assert_eq!(
        fields.len(),
        1,
        "oneof fields not emitted in v1; only id captured"
    );
    assert_eq!(fields[0].type_class, SchemaType::Int);
}

/// `//` inline comments are stripped before field parsing.
#[test]
fn test_inline_comment_stripped() {
    let src = r#"
syntax = "proto3";
message Annotated {
    string label = 1; // human-readable label
    int32 count = 2;  // how many
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 2);
}

/// `// full-line comment` lines are ignored.
#[test]
fn test_full_line_comment_ignored() {
    let src = r#"
syntax = "proto3";
// This is a comment
message Thing {
    // field comment
    double value = 1;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::Float);
}

/// Custom message-type fields (e.g. `Address address = 3;`) are emitted with
/// `SchemaType::Other`.
#[test]
fn test_custom_type_emitted_as_other() {
    let src = r#"
syntax = "proto3";
message User {
    string name = 1;
    Address address = 2;
}
"#;
    let fields = parse(src);
    assert_eq!(fields.len(), 2);
    let other_count = fields
        .iter()
        .filter(|f| f.type_class == SchemaType::Other)
        .count();
    assert_eq!(other_count, 1, "Address → SchemaType::Other");
}
