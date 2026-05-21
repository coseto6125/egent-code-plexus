//! T4-3: SQLAlchemy declarative ORM column field extraction tests.
//!
//! Covers both idioms:
//!   A) Classic `Column()` declarative (SQLAlchemy 1.x / 2.x compatible)
//!   B) `Mapped[T]` typed declarative (SQLAlchemy 2.0 style)
//!
//! All tests call `extract_schema_fields` directly with a local `StringPool`
//! so strings are resolved before the pool is dropped.  The `parse_file`
//! wiring stores fields in `LocalGraph.schema_fields`; builder-side re-interning
//! is a separate future pass (see TODO in parser.rs).

use ecp_analyzer::python::schema_extractors::SQLALCHEMY_CONFIG;
use ecp_analyzer::schema_field::extract_schema_fields;
use ecp_core::analyzer::types::{FrameworkId, RawImport, RawSchemaField, SchemaType};
use ecp_core::pool::StringPool;
use tree_sitter::{Parser, Query};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Inline-compiled SQLAlchemy fragment from `frameworks.scm`. Tests use this
/// fragment directly instead of the production-merged query so they don't
/// depend on `PythonProvider::new()` query-build behaviour.
const SQLA_QUERY: &str = r#"
(class_definition
  name: (identifier) @sqlalchemy.owner
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @sqlalchemy.field
        right: (call
          function: (identifier) @_col (#eq? @_col "Column")
          arguments: (argument_list
            . (identifier) @sqlalchemy.type))))))

(class_definition
  name: (identifier) @sqlalchemy.owner
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @sqlalchemy.field
        type: (type
          (generic_type
            (identifier) @_mapped (#eq? @_mapped "Mapped")
            (type_parameter
              (type (identifier) @sqlalchemy.type))))
        right: (call
          function: (identifier) @_mc (#eq? @_mc "mapped_column"))))))
"#;

/// Parse `src`, fabricate `RawImport`s from `import_sources` (only the
/// `source` field is checked by `has_import_from`), then run the dispatcher.
/// Returns the extracted fields plus the pool that owns the interned strings.
fn run(src: &str, import_sources: &[&str]) -> (Vec<RawSchemaField>, StringPool) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(&lang, SQLA_QUERY).expect("query compile");
    let imports: Vec<RawImport> = import_sources
        .iter()
        .map(|s| RawImport {
            source: (*s).to_string(),
            imported_name: "*".to_string(),
            alias: None,
            binding_kind: None,
        })
        .collect();
    let mut pool = StringPool::new();
    let fields = extract_schema_fields(
        &tree,
        src.as_bytes(),
        &query,
        &[SQLALCHEMY_CONFIG],
        &imports,
        &mut pool,
    );
    (fields, pool)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `Column(String)` → SchemaType::String, owner = class name, framework = SqlAlchemy.
#[test]
fn test_column_string_type() {
    let src =
        "from sqlalchemy import Column, String\n\nclass User(Base):\n    name = Column(String)\n";
    let (fields, pool) = run(src, &["sqlalchemy"]);

    assert_eq!(fields.len(), 1);
    let pool_bytes = pool.bytes.as_slice();
    assert_eq!(fields[0].type_class, SchemaType::String);
    assert_eq!(fields[0].name.resolve(pool_bytes), "name");
    assert_eq!(fields[0].owner_class.resolve(pool_bytes), "User");
    assert_eq!(fields[0].framework, FrameworkId::SqlAlchemy);
}

/// `Column(Integer, primary_key=True)` → SchemaType::Int (keyword args are ignored).
#[test]
fn test_column_integer_type() {
    let src =
        "from sqlalchemy import Column, Integer\n\nclass User(Base):\n    id = Column(Integer, primary_key=True)\n";
    let (fields, pool) = run(src, &["sqlalchemy"]);

    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::Int);
    assert_eq!(fields[0].name.resolve(pool.bytes.as_slice()), "id");
}

/// `Column(String(50))` — the first arg is a *call* not an identifier.
/// The query's `. (identifier) @sqlalchemy.type` anchor requires a bare
/// identifier as the first positional arg, so `String(50)` is NOT captured.
/// This is a documented BlindSpot; field is not emitted.
#[test]
fn test_column_with_size_arg_not_captured() {
    let src = "from sqlalchemy import Column, String\n\nclass Post(Base):\n    title = Column(String(50))\n";
    let (fields, _pool) = run(src, &["sqlalchemy"]);

    assert!(
        fields.is_empty(),
        "Column(String(50)) first arg is a call, not a bare identifier — not captured"
    );
}

/// `Mapped[int]` → SchemaType::Int via the typed declarative idiom.
#[test]
fn test_mapped_column_typed_int() {
    let src = "from sqlalchemy.orm import Mapped, mapped_column\n\nclass User(Base):\n    id: Mapped[int] = mapped_column(primary_key=True)\n";
    let (fields, pool) = run(src, &["sqlalchemy.orm"]);

    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::Int);
    let pool_bytes = pool.bytes.as_slice();
    assert_eq!(fields[0].name.resolve(pool_bytes), "id");
    assert_eq!(fields[0].owner_class.resolve(pool_bytes), "User");
    assert_eq!(fields[0].framework, FrameworkId::SqlAlchemy);
}

/// `Mapped[str]` → SchemaType::String.
#[test]
fn test_mapped_column_str() {
    let src = "from sqlalchemy.orm import Mapped, mapped_column\n\nclass User(Base):\n    name: Mapped[str] = mapped_column()\n";
    let (fields, pool) = run(src, &["sqlalchemy.orm"]);

    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::String);
    assert_eq!(fields[0].name.resolve(pool.bytes.as_slice()), "name");
}

/// No sqlalchemy import → import gate blocks all emission.
#[test]
fn test_no_sqlalchemy_import_no_emit() {
    let src = "class User(Base):\n    name = Column(String)\n";
    let (fields, _pool) = run(src, &[]);

    assert!(
        fields.is_empty(),
        "import gate must block emission without sqlalchemy import"
    );
}

/// Two classes both with Column fields → correct owner per field.
#[test]
fn test_multiple_classes() {
    let src = "from sqlalchemy import Column, Integer, String\n\nclass User(Base):\n    id = Column(Integer)\n    name = Column(String)\n\nclass Post(Base):\n    id = Column(Integer)\n    title = Column(String)\n";
    let (fields, pool) = run(src, &["sqlalchemy"]);

    assert_eq!(fields.len(), 4, "User(2) + Post(2) = 4 fields");
    let pool_bytes = pool.bytes.as_slice();
    let owners: Vec<&str> = fields
        .iter()
        .map(|f| f.owner_class.resolve(pool_bytes))
        .collect();
    assert_eq!(owners.iter().filter(|&&o| o == "User").count(), 2);
    assert_eq!(owners.iter().filter(|&&o| o == "Post").count(), 2);
}

/// `relationship("Post")` must NOT emit a schema field.
/// The query only matches `Column()` or `mapped_column()` call forms.
#[test]
fn test_ignores_relationship() {
    let src = "from sqlalchemy import Column, Integer\nfrom sqlalchemy.orm import relationship\n\nclass User(Base):\n    id = Column(Integer)\n    posts = relationship(\"Post\")\n";
    let (fields, pool) = run(src, &["sqlalchemy"]);

    assert_eq!(
        fields.len(),
        1,
        "relationship() must not emit a schema field"
    );
    assert_eq!(fields[0].name.resolve(pool.bytes.as_slice()), "id");
}

/// `parse_file` integration smoke test: PythonProvider populates
/// `LocalGraph.schema_fields` when sqlalchemy is imported.
#[test]
fn test_parse_file_populates_schema_fields() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "from sqlalchemy import Column, Integer, String\n\nclass User(Base):\n    id = Column(Integer)\n    name = Column(String)\n";
    let provider = PythonProvider::new().expect("provider init");
    let local = provider
        .parse_file("models.py".as_ref(), src.as_bytes())
        .expect("parse_file");

    let fields = local.schema_fields.expect("schema_fields must be Some");
    assert_eq!(fields.len(), 2, "User has 2 columns");
    assert!(
        fields
            .iter()
            .all(|f| f.framework == FrameworkId::SqlAlchemy),
        "all fields must have SqlAlchemy framework"
    );
}

/// No sqlalchemy import → `LocalGraph.schema_fields` is `None`.
#[test]
fn test_parse_file_no_import_schema_fields_none() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "class User(Base):\n    id = Column(Integer)\n";
    let provider = PythonProvider::new().expect("provider init");
    let local = provider
        .parse_file("plain.py".as_ref(), src.as_bytes())
        .expect("parse_file");

    assert!(
        local.schema_fields.is_none(),
        "no sqlalchemy import → schema_fields must be None"
    );
}

/// BlindSpot: `Column(String(50))` — first arg is a parameterised call, not a
/// bare identifier.  The `. (identifier)` anchor in the tree-sitter query
/// rejects call nodes as the first positional argument, so the type is lost.
/// A smarter capture could unwrap `(call function: (identifier) @type)` but
/// that would also match multi-arg calls like `Column(ForeignKey("t.id"))`.
///
/// Similarly, `Column(types.JSON)` (namespace attribute access) is not
/// captured because the first arg is an `attribute` node, not an `identifier`.
#[test]
#[ignore = "BlindSpot: Column(String(50)) and Column(Namespace.Type) — first arg is a call/attribute, not a bare identifier; requires smarter type unwrapping"]
fn test_column_parameterised_type_blind_spot() {
    let src = "from sqlalchemy import Column, String\n\nclass Post(Base):\n    title = Column(String(50))\n    body = Column(String(255))\n";
    let (fields, _pool) = run(src, &["sqlalchemy"]);

    assert_eq!(
        fields.len(),
        2,
        "ideally 2 fields but BlindSpot prevents capture"
    );
}
