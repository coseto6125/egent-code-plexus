//! T4-4: TypeScript `interface` property extraction tests.
//!
//! Most tests call `extract_schema_fields` directly with a local `StringPool`
//! so strings are resolved before the pool is dropped.  The last two tests use
//! `TypeScriptProvider::parse_file` as an integration smoke check.
//!
//! Import gate is empty for TS interfaces (language built-in, no framework
//! import required); `extract_schema_fields` treats `&[]` as vacuously satisfied.

use ecp_analyzer::schema_field::extract_schema_fields;
use ecp_analyzer::typescript::schema_extractors::TS_INTERFACE_CONFIG;
use ecp_core::analyzer::types::{FrameworkId, RawSchemaField, SchemaType};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Inline query fragment — matches the same pattern as `frameworks.scm`
/// but compiled standalone so unit tests don't depend on the query-merge step
/// in `TypeScriptProvider::new()`.
// The TS grammar aliases `object_type` as `interface_body` when used as the
// body of an `interface_declaration` — tree-sitter exposes the alias name,
// not the underlying rule name, so `(interface_body ...)` is correct here.
const INTERFACE_QUERY: &str = r#"
(interface_declaration
  name: (type_identifier) @ts.owner
  body: (interface_body
    (property_signature
      name: (property_identifier) @ts.field
      type: (type_annotation
        (predefined_type) @ts.type))))
"#;

/// Run the TS interface dispatcher against `src`.
/// Returns extracted fields + the pool that owns their string bytes.
fn run(src: &str) -> (Vec<RawSchemaField>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, INTERFACE_QUERY).expect("query compile");
    // No imports needed — empty gate is always active.
    let mut pool = StringPool::new();
    let fields = extract_schema_fields(
        &tree,
        src.as_bytes(),
        &query,
        &[TS_INTERFACE_CONFIG],
        &[],
        &mut pool,
    );
    (fields, pool)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_simple_string_field() {
    let src = "interface User { name: string; }";
    let (fields, pool) = run(src);

    assert_eq!(fields.len(), 1);
    let f = &fields[0];
    let bytes = pool.bytes.as_slice();
    assert_eq!(f.name.resolve(bytes), "name");
    assert_eq!(f.type_class, SchemaType::String);
    assert_eq!(f.owner_class.resolve(bytes), "User");
    assert_eq!(f.framework, FrameworkId::TypeScriptInterface);
}

#[test]
fn test_number_field() {
    let src = "interface Item { age: number; }";
    let (fields, pool) = run(src);

    assert_eq!(fields.len(), 1);
    let f = &fields[0];
    let bytes = pool.bytes.as_slice();
    assert_eq!(f.name.resolve(bytes), "age");
    // classify_ts_type maps `number` → SchemaType::Int
    assert_eq!(f.type_class, SchemaType::Int);
    assert_eq!(f.owner_class.resolve(bytes), "Item");
}

#[test]
fn test_boolean_field() {
    let src = "interface Config { active: boolean; }";
    let (fields, pool) = run(src);

    assert_eq!(fields.len(), 1);
    let bytes = pool.bytes.as_slice();
    assert_eq!(fields[0].name.resolve(bytes), "active");
    assert_eq!(fields[0].type_class, SchemaType::Bool);
}

#[test]
fn test_date_field() {
    // `Date` is not a predefined_type in TS grammar — it is a type_identifier.
    // This test verifies that `Date` fields are NOT captured (only predefined_type
    // nodes match the query), which is correct: classify_ts_type("Date") = Datetime,
    // but the grammar node is `type_identifier`, not `predefined_type`.
    // The query intentionally restricts to `predefined_type` to avoid capturing
    // arbitrary type references.  Date support requires a separate query arm.
    let src = "interface Event { created: Date; }";
    let (fields, _pool) = run(src);
    // Date is a type_identifier, not a predefined_type — not captured by this query.
    assert!(
        fields.is_empty(),
        "Date is a type_identifier, not predefined_type — not captured by this query arm"
    );
}

#[test]
fn test_union_type_other() {
    // `string | null` is a `union_type` node, not `predefined_type`.
    // The query only captures `predefined_type` → this field is not emitted.
    let src = "interface Contact { email: string | null; }";
    let (fields, _pool) = run(src);
    assert!(
        fields.is_empty(),
        "union_type does not match predefined_type capture — field not emitted"
    );
}

#[test]
fn test_array_type_other() {
    // `string[]` is an `array_type` node, not `predefined_type`.
    let src = "interface Post { tags: string[]; }";
    let (fields, _pool) = run(src);
    assert!(
        fields.is_empty(),
        "array_type does not match predefined_type capture — field not emitted"
    );
}

#[test]
fn test_multiple_interfaces() {
    let src = "interface Foo { x: number; label: string; } interface Bar { active: boolean; }";
    let (fields, pool) = run(src);

    assert_eq!(fields.len(), 3, "Foo(2) + Bar(1) = 3 total fields");
    let bytes = pool.bytes.as_slice();

    let owners: Vec<&str> = fields
        .iter()
        .map(|f| f.owner_class.resolve(bytes))
        .collect();
    assert!(owners.contains(&"Foo"), "Foo fields must appear");
    assert!(owners.contains(&"Bar"), "Bar fields must appear");

    let foo_fields: Vec<&str> = fields
        .iter()
        .filter(|f| f.owner_class.resolve(bytes) == "Foo")
        .map(|f| f.name.resolve(bytes))
        .collect();
    assert!(foo_fields.contains(&"x"));
    assert!(foo_fields.contains(&"label"));

    let bar_fields: Vec<&str> = fields
        .iter()
        .filter(|f| f.owner_class.resolve(bytes) == "Bar")
        .map(|f| f.name.resolve(bytes))
        .collect();
    assert!(bar_fields.contains(&"active"));
}

#[test]
fn test_nested_interface_does_not_emit_extras() {
    // `user: User` — `User` is a `type_identifier`, not `predefined_type`.
    // Only the `predefined_type` arm fires; `user` field is not emitted.
    let src = "interface Wrapper { user: User; count: number; }";
    let (fields, pool) = run(src);

    // Only `count: number` (predefined_type) is captured; `user: User` is not.
    assert_eq!(fields.len(), 1, "only predefined_type fields are captured");
    let bytes = pool.bytes.as_slice();
    assert_eq!(fields[0].name.resolve(bytes), "count");
    assert_eq!(fields[0].type_class, SchemaType::Int);
    assert_eq!(fields[0].owner_class.resolve(bytes), "Wrapper");
}

/// BlindSpot: optional properties (`name?: string`) use `property_signature`
/// with an optional marker — tree-sitter represents these identically to
/// required properties, so the query DOES capture them.  This test documents
/// the current behaviour (optional fields are captured) rather than a gap.
#[test]
fn test_optional_property_is_captured() {
    let src = "interface User { name?: string; }";
    let (fields, pool) = run(src);
    // Optional properties ARE captured — `?` is a modifier on the
    // property_signature, not on the name node, so the query fires.
    assert_eq!(fields.len(), 1);
    let bytes = pool.bytes.as_slice();
    assert_eq!(fields[0].name.resolve(bytes), "name");
    assert_eq!(fields[0].type_class, SchemaType::String);
}

/// BlindSpot: `type` alias declarations (`type Alias = { field: string }`) are
/// not captured.  They use `type_alias_declaration` + `object_type` which is a
/// different grammar shape than `interface_declaration`.  T4-5 should add a
/// separate query arm for type aliases.
#[test]
#[ignore = "BlindSpot: type alias object literals require a separate query arm (T4-5)"]
fn test_type_alias_not_captured() {
    let src = "type Point = { x: number; y: number; }";
    let (fields, _pool) = run(src);
    // Currently zero — type aliases use type_alias_declaration, not
    // interface_declaration.  T4-5 tracks this gap.
    assert!(
        fields.is_empty(),
        "type aliases not captured by interface query — T4-5 gap"
    );
}

// ---------------------------------------------------------------------------
// parse_file integration smoke tests
// ---------------------------------------------------------------------------

#[test]
fn test_parse_file_populates_schema_fields() {
    use ecp_analyzer::typescript::TypeScriptProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "interface Product { name: string; price: number; }";
    let provider = TypeScriptProvider::new().expect("provider init");
    let local = provider
        .parse_file("types.ts".as_ref(), src.as_bytes())
        .expect("parse_file");

    let fields = local.schema_fields.expect("schema_fields must be Some");
    assert_eq!(fields.len(), 2, "Product has 2 predefined-type fields");
    assert!(fields
        .iter()
        .all(|f| f.framework == FrameworkId::TypeScriptInterface));
}

#[test]
fn test_parse_file_no_interface_schema_fields_none() {
    use ecp_analyzer::typescript::TypeScriptProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "function add(a: number, b: number): number { return a + b; }";
    let provider = TypeScriptProvider::new().expect("provider init");
    let local = provider
        .parse_file("utils.ts".as_ref(), src.as_bytes())
        .expect("parse_file");

    assert!(
        local.schema_fields.is_none(),
        "no interface fields → schema_fields must be None"
    );
}
