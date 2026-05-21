//! T4-1 unit tests: config-table dispatch + import-gate filtering.
//!
//! Deliberately framework-agnostic — no Pydantic / SQLAlchemy / TS specifics.
//! Those belong to T4-2..T4-6.

use ecp_analyzer::schema_field::{extract_schema_fields, SchemaFieldConfig};
use ecp_core::analyzer::types::{FrameworkId, RawImport, SchemaType};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build a tree-sitter tree for the snippet using the Python grammar.
fn python_tree(src: &str) -> (tree_sitter::Tree, tree_sitter::Language) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser
        .parse(src.as_bytes(), None)
        .expect("parse returned None");
    (tree, lang)
}

/// Fabricate a `RawImport` that looks like `from <source> import *`.
fn fake_import(source: &str) -> RawImport {
    RawImport {
        source: source.to_string(),
        imported_name: "*".to_string(),
        alias: None,
        binding_kind: None,
    }
}

// ---------------------------------------------------------------------------
// Query used by all dispatch tests.
//
// Pattern: Python class body assignments with a type comment, e.g.
//   class MyModel:
//       field_a: str
//
// Captures: @owner = class name, @field = attribute name, @type = type text.
// ---------------------------------------------------------------------------
const FIELD_QUERY: &str = r#"
(class_definition
  name: (identifier) @owner
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @field
        type: (_) @type))))
"#;

// ---------------------------------------------------------------------------
// Classifier functions referenced by the test configs.
// ---------------------------------------------------------------------------

fn classify_str_int(raw: &str) -> SchemaType {
    match raw {
        "str" | "String" => SchemaType::String,
        "int" | "Integer" => SchemaType::Int,
        _ => SchemaType::Other,
    }
}

// ---------------------------------------------------------------------------
// Minimal configs used across tests — no real framework imports.
// ---------------------------------------------------------------------------

const CONFIG_A: SchemaFieldConfig = SchemaFieldConfig {
    framework: FrameworkId::Pydantic,
    owner_capture: "owner",
    name_capture: "field",
    type_capture: "type",
    import_gate: &["lib-alpha"],
    type_classifier: classify_str_int,
};

const CONFIG_B: SchemaFieldConfig = SchemaFieldConfig {
    framework: FrameworkId::SqlAlchemy,
    owner_capture: "owner",
    name_capture: "field",
    type_capture: "type",
    import_gate: &["lib-beta"],
    type_classifier: classify_str_int,
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Two configs with different import gates; only the one whose gate matches
/// the imports slice should label emitted fields.
#[test]
fn test_config_driven_dispatch_picks_right_framework_label() {
    let src = "class MyModel:\n    name: str = None\n";
    let (tree, lang) = python_tree(src);
    let query = Query::new(&lang, FIELD_QUERY).expect("query compile");
    let mut pool = StringPool::new();

    // Only lib-alpha import → CONFIG_A should fire, CONFIG_B should not.
    let imports = vec![fake_import("lib-alpha")];
    let fields = extract_schema_fields(
        &tree,
        src.as_bytes(),
        &query,
        &[CONFIG_A, CONFIG_B],
        &imports,
        &mut pool,
    );

    assert_eq!(fields.len(), 1, "expected exactly one field");
    assert_eq!(fields[0].framework, FrameworkId::Pydantic);

    // Flip: only lib-beta import → CONFIG_B fires.
    let imports_b = vec![fake_import("lib-beta")];
    let mut pool2 = StringPool::new();
    let fields_b = extract_schema_fields(
        &tree,
        src.as_bytes(),
        &query,
        &[CONFIG_A, CONFIG_B],
        &imports_b,
        &mut pool2,
    );

    assert_eq!(fields_b.len(), 1);
    assert_eq!(fields_b[0].framework, FrameworkId::SqlAlchemy);
}

/// No required import present → zero fields emitted (gate blocks dispatch).
#[test]
fn test_import_gate_negative_drops_capture() {
    let src = "class MyModel:\n    name: str = None\n";
    let (tree, lang) = python_tree(src);
    let query = Query::new(&lang, FIELD_QUERY).expect("query compile");
    let mut pool = StringPool::new();

    // Imports for an unrelated library — neither gate satisfied.
    let imports = vec![fake_import("unrelated-lib")];
    let fields = extract_schema_fields(
        &tree,
        src.as_bytes(),
        &query,
        &[CONFIG_A, CONFIG_B],
        &imports,
        &mut pool,
    );

    assert!(fields.is_empty(), "import gate must block all extraction");
}

/// Type strings are routed to the correct SchemaType via the classifier fn.
#[test]
fn test_classifier_routes_type_strings() {
    use ecp_analyzer::schema_field::extract::{classify_python_type, classify_ts_type};

    // Python classifier
    assert_eq!(classify_python_type("str"), SchemaType::String);
    assert_eq!(classify_python_type("String"), SchemaType::String);
    assert_eq!(classify_python_type("Text"), SchemaType::String);
    assert_eq!(classify_python_type("int"), SchemaType::Int);
    assert_eq!(classify_python_type("Integer"), SchemaType::Int);
    assert_eq!(classify_python_type("float"), SchemaType::Float);
    assert_eq!(classify_python_type("Float"), SchemaType::Float);
    assert_eq!(classify_python_type("bool"), SchemaType::Bool);
    assert_eq!(classify_python_type("Boolean"), SchemaType::Bool);
    assert_eq!(classify_python_type("datetime"), SchemaType::Datetime);
    assert_eq!(classify_python_type("DateTime"), SchemaType::Datetime);
    assert_eq!(classify_python_type("dict"), SchemaType::Json);
    assert_eq!(classify_python_type("JSON"), SchemaType::Json);
    assert_eq!(classify_python_type("SomeOrmType"), SchemaType::Other);
    assert_eq!(classify_python_type(""), SchemaType::Other);

    // TypeScript classifier
    assert_eq!(classify_ts_type("string"), SchemaType::String);
    assert_eq!(classify_ts_type("number"), SchemaType::Int);
    assert_eq!(classify_ts_type("bigint"), SchemaType::Int);
    assert_eq!(classify_ts_type("boolean"), SchemaType::Bool);
    assert_eq!(classify_ts_type("Date"), SchemaType::Datetime);
    assert_eq!(classify_ts_type("object"), SchemaType::Json);
    assert_eq!(classify_ts_type("unknown"), SchemaType::Other);
}
