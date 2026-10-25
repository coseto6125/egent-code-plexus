//! T4-6: OpenAPI 3.x / Swagger 2.0 schema-field extraction tests.
//!
//! All tests call `OpenApiProvider::parse_file`, the provider a sniffed
//! OpenAPI YAML routes to (C5), to verify end-to-end field emission.
//! String content is verified via the provider interface by round-tripping
//! through `parse_file`, then checking `schema_fields`.

use ecp_analyzer::openapi::OpenApiProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, SchemaType};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn parse_openapi(filename: &str, src: &str) -> Vec<ecp_core::analyzer::types::RawSchemaField> {
    let provider = OpenApiProvider::new().expect("provider init");
    let local = provider
        .parse_file(filename.as_ref(), src.as_bytes())
        .expect("parse_file");
    local
        .schema_fields
        .map(|b| b.into_vec())
        .unwrap_or_default()
}

fn parse_yaml(src: &str) -> Vec<ecp_core::analyzer::types::RawSchemaField> {
    parse_openapi("openapi.yaml", src)
}

// ── Positive: OpenAPI 3.x YAML ────────────────────────────────────────────────

#[test]
fn yaml_components_schemas_emits_fields() {
    let src = r#"openapi: "3.0.3"
info:
  title: Test API
  version: "1.0"
components:
  schemas:
    User:
      type: object
      properties:
        id:
          type: integer
        name:
          type: string
        active:
          type: boolean
"#;
    let fields = parse_yaml(src);
    assert_eq!(fields.len(), 3, "expected 3 fields; got {fields:?}");
    assert!(fields.iter().all(|f| f.framework == FrameworkId::OpenApi));
    assert!(fields.iter().all(|f| f.owner_class.as_ref() == "User"));

    let by_name = |n: &str| fields.iter().find(|f| f.name.as_ref() == n).unwrap();
    assert_eq!(by_name("id").type_class, SchemaType::Int);
    assert_eq!(by_name("name").type_class, SchemaType::String);
    assert_eq!(by_name("active").type_class, SchemaType::Bool);
}

// ── Positive: OpenAPI 3.x JSON ────────────────────────────────────────────────

#[test]
fn json_components_schemas_emits_fields() {
    let src = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Test", "version": "1.0" },
  "components": {
    "schemas": {
      "Product": {
        "type": "object",
        "properties": {
          "price": { "type": "number" },
          "tags": { "type": "array" }
        }
      }
    }
  }
}"#;
    let fields = parse_openapi("spec.json", src);
    assert_eq!(fields.len(), 2, "expected 2 fields; got {fields:?}");
    assert!(fields.iter().all(|f| f.framework == FrameworkId::OpenApi));
    assert!(fields.iter().all(|f| f.owner_class.as_ref() == "Product"));

    let by_name = |n: &str| fields.iter().find(|f| f.name.as_ref() == n).unwrap();
    assert_eq!(by_name("price").type_class, SchemaType::Float);
    assert_eq!(by_name("tags").type_class, SchemaType::Json);
}

// ── Positive: Swagger 2.0 YAML ───────────────────────────────────────────────

#[test]
fn swagger2_definitions_emits_fields() {
    let src = r#"swagger: "2.0"
info:
  title: Swagger 2 Test
  version: "1.0"
definitions:
  Order:
    type: object
    properties:
      order_id:
        type: integer
      status:
        type: string
      metadata:
        type: object
"#;
    let fields = parse_yaml(src);
    assert_eq!(fields.len(), 3, "expected 3 fields; got {fields:?}");
    assert!(fields.iter().all(|f| f.framework == FrameworkId::Swagger));
    assert!(fields.iter().all(|f| f.owner_class.as_ref() == "Order"));

    let by_name = |n: &str| fields.iter().find(|f| f.name.as_ref() == n).unwrap();
    assert_eq!(by_name("order_id").type_class, SchemaType::Int);
    assert_eq!(by_name("status").type_class, SchemaType::String);
    assert_eq!(by_name("metadata").type_class, SchemaType::Json);
}

// ── Positive: date-time format → Datetime ────────────────────────────────────

#[test]
fn date_time_format_yields_datetime_typeclass() {
    let src = r#"openapi: "3.0.3"
info:
  title: Datetime Test
  version: "1.0"
components:
  schemas:
    Event:
      type: object
      properties:
        created_at:
          type: string
          format: date-time
        label:
          type: string
"#;
    let fields = parse_yaml(src);
    let by_name = |n: &str| fields.iter().find(|f| f.name.as_ref() == n).unwrap();
    assert_eq!(
        by_name("created_at").type_class,
        SchemaType::Datetime,
        "string+date-time must map to Datetime"
    );
    assert_eq!(
        by_name("label").type_class,
        SchemaType::String,
        "plain string must map to String"
    );
}

// ── Negative: non-OpenAPI YAML emits zero fields ──────────────────────────────

#[test]
fn non_openapi_yaml_emits_zero_fields() {
    // A k8s Deployment manifest — must not emit any SchemaField nodes.
    let src = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-app
spec:
  replicas: 3
  selector:
    matchLabels:
      app: my-app
  template:
    metadata:
      labels:
        app: my-app
    spec:
      containers:
        - name: app
          image: my-app:latest
"#;
    let fields = parse_yaml(src);
    assert!(
        fields.is_empty(),
        "k8s manifest must not emit any SchemaField; got {fields:?}"
    );
}

// ── Negative: paths inline schemas not emitted (v1 scope guard) ───────────────

#[test]
fn paths_inline_schemas_not_emitted_v1() {
    // Only `components.schemas` is scanned; inline schemas under `paths` must
    // not be picked up in v1.
    let src = r#"openapi: "3.0.3"
info:
  title: Inline Schema Test
  version: "1.0"
paths:
  /users:
    get:
      summary: Get users
      responses:
        "200":
          description: success
          content:
            application/json:
              schema:
                type: object
                properties:
                  email:
                    type: string
                  age:
                    type: integer
"#;
    // The file has `openapi:` marker but no `components.schemas` → zero fields.
    let fields = parse_yaml(src);
    assert!(
        fields.is_empty(),
        "paths inline schemas must not be emitted in v1; got {fields:?}"
    );
}

// ── Multiple schemas in one file ─────────────────────────────────────────────

#[test]
fn multiple_schemas_all_emitted() {
    let src = r#"openapi: "3.0.3"
info:
  title: Multi-schema Test
  version: "1.0"
components:
  schemas:
    User:
      type: object
      properties:
        id:
          type: integer
    Address:
      type: object
      properties:
        street:
          type: string
        zip:
          type: string
"#;
    let fields = parse_yaml(src);
    assert_eq!(
        fields.len(),
        3,
        "expected id + street + zip; got {fields:?}"
    );
    let user_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.owner_class.as_ref() == "User")
        .collect();
    let addr_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.owner_class.as_ref() == "Address")
        .collect();
    assert_eq!(user_fields.len(), 1);
    assert_eq!(addr_fields.len(), 2);
}

// ── Source lines: each schema and field carries the line of its key ─────────

/// `(start_row, end_row)` of the Struct node `name` in an OpenAPI parse.
fn struct_rows(filename: &str, src: &str, name: &str) -> (u32, u32) {
    let local = OpenApiProvider::new()
        .unwrap()
        .parse_file(filename.as_ref(), src.as_bytes())
        .unwrap();
    let node = local.nodes.iter().find(|n| n.name == name).unwrap();
    (node.span.0, node.span.2)
}

fn field_row(fields: &[ecp_core::analyzer::types::RawSchemaField], owner: &str, name: &str) -> u32 {
    fields
        .iter()
        .find(|f| f.owner_class.as_ref() == owner && f.name.as_ref() == name)
        .unwrap()
        .span
        .0
}

#[test]
fn test_extract_fields_yaml_schema_and_field_carry_key_rows() {
    // `id` also appears nested under `address` before `Pet.id`: the field row
    // must come from the `properties` level, not the first `id:` text.
    let src = "openapi: 3.0.0\ninfo:\n  title: t\n  version: \"1\"\ncomponents:\n  schemas:\n    Pet:\n      type: object\n      properties:\n        address:\n          type: object\n          properties:\n            id:\n              type: string\n        id:\n          type: integer\n    Owner:\n      properties:\n        name:\n          type: string\n";
    assert_eq!(struct_rows("openapi.yaml", src, "Pet"), (6, 15));
    assert_eq!(struct_rows("openapi.yaml", src, "Owner"), (16, 19));
    let fields = parse_openapi("openapi.yaml", src);
    assert_eq!(field_row(&fields, "Pet", "address"), 9);
    assert_eq!(field_row(&fields, "Pet", "id"), 14);
    assert_eq!(field_row(&fields, "Owner", "name"), 18);
}

#[test]
fn test_extract_fields_json_schema_and_field_carry_key_rows() {
    let src = "{\n  \"openapi\": \"3.0.0\",\n  \"info\": {\"title\": \"t\", \"version\": \"1\"},\n  \"components\": {\n    \"schemas\": {\n      \"Pet\": {\n        \"type\": \"object\",\n        \"properties\": {\n          \"id\": {\"type\": \"integer\"}\n        }\n      }\n    }\n  }\n}\n";
    assert_eq!(struct_rows("openapi.json", src, "Pet"), (5, 10));
    let fields = parse_openapi("openapi.json", src);
    assert_eq!(field_row(&fields, "Pet", "id"), 8);
}

#[test]
fn test_extract_fields_swagger_definitions_carry_key_rows() {
    let src = "swagger: \"2.0\"\ninfo:\n  title: t\n  version: \"1\"\ndefinitions:\n  Tag:\n    properties:\n      label:\n        type: string\n";
    assert_eq!(struct_rows("swagger.yaml", src, "Tag"), (5, 8));
    let fields = parse_openapi("swagger.yaml", src);
    assert_eq!(field_row(&fields, "Tag", "label"), 7);
}

// Each row puts YAML syntax between a key and its mapping, or before the
// root mapping. A walk that takes the first child for the mapping loses the
// row, and the node falls back to the (0, 0, 0, 0) sentinel (line 1).

#[test]
fn test_extract_fields_anchor_before_schema_mapping_carries_key_rows() {
    let src = "openapi: 3.0.0\ncomponents:\n  schemas:\n    Pet: &pet\n      properties: &pet_props\n        id:\n          type: integer\n";
    assert_eq!(struct_rows("openapi.yaml", src, "Pet"), (3, 6));
    let fields = parse_openapi("openapi.yaml", src);
    assert_eq!(field_row(&fields, "Pet", "id"), 5);
}

#[test]
fn test_extract_fields_tag_before_schema_mapping_carries_key_rows() {
    let src = "openapi: 3.0.0\ncomponents:\n  schemas:\n    Pet: !!map\n      properties:\n        id:\n          type: integer\n";
    assert_eq!(struct_rows("openapi.yaml", src, "Pet"), (3, 6));
    let fields = parse_openapi("openapi.yaml", src);
    assert_eq!(field_row(&fields, "Pet", "id"), 5);
}

#[test]
fn test_extract_fields_comment_after_document_start_carries_key_rows() {
    let src = "---\n# Pet store\nopenapi: 3.0.0\ncomponents:\n  schemas:\n    Pet:\n      # the pet\n      properties:\n        id:\n          type: integer\n";
    assert_eq!(struct_rows("openapi.yaml", src, "Pet"), (5, 9));
    let fields = parse_openapi("openapi.yaml", src);
    assert_eq!(field_row(&fields, "Pet", "id"), 8);
}

#[test]
fn test_extract_fields_yaml_directive_before_root_carries_key_rows() {
    let src = "%YAML 1.2\n---\nopenapi: 3.0.0\ncomponents:\n  schemas:\n    Pet:\n      properties:\n        id:\n          type: integer\n";
    assert_eq!(struct_rows("openapi.yaml", src, "Pet"), (5, 8));
    let fields = parse_openapi("openapi.yaml", src);
    assert_eq!(field_row(&fields, "Pet", "id"), 7);
}
