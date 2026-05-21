//! T4-2: Pydantic v1+v2 BaseModel field extraction tests.
//!
//! Post-T4-7 refactor: `RawSchemaField` now stores owned `Box<str>` so
//! `.name` / `.owner_class` are directly readable as `&str` — no pool plumbing.

use ecp_analyzer::python::schema_extractors::PYDANTIC_CONFIG;
use ecp_analyzer::schema_field::extract_schema_fields;
use ecp_core::analyzer::types::{FrameworkId, RawImport, RawSchemaField, SchemaType};
use tree_sitter::{Parser, Query};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn pydantic_import() -> RawImport {
    RawImport {
        source: "pydantic".to_string(),
        imported_name: "BaseModel".to_string(),
        alias: None,
        binding_kind: None,
    }
}

/// Run the Pydantic dispatcher against `src`. Returns the extracted fields
/// with owned strings (no pool teardown concerns).
///
/// `with_import` toggles the `pydantic` import-gate; tests use `false` to
/// verify gating, `true` for happy paths. The query is the Pydantic fragment
/// from `frameworks.scm`, compiled inline so unit tests don't depend on the
/// production-query merge step in `PythonProvider::new()`.
fn run(src: &str, with_import: bool) -> Vec<RawSchemaField> {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).expect("set_language");
    let tree = parser.parse(src.as_bytes(), None).expect("parse");
    let query = Query::new(
        &lang,
        r#"
(class_definition
  name: (identifier) @pydantic.owner
  superclasses: (argument_list (identifier) @_super)
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @pydantic.field
        type: (type) @pydantic.type))))
(#eq? @_super "BaseModel")
"#,
    )
    .expect("query compile");
    let imports = if with_import {
        vec![pydantic_import()]
    } else {
        Vec::new()
    };
    extract_schema_fields(&tree, src.as_bytes(), &query, &[PYDANTIC_CONFIG], &imports)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Happy path: two plain typed fields → two RawSchemaField with correct names,
/// types, owner, and framework.
#[test]
fn test_happy_path_two_fields() {
    let src =
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    name: str\n    age: int\n";
    let fields = run(src, true);

    assert_eq!(fields.len(), 2, "expected two fields for User");

    let by_name: std::collections::HashMap<&str, &_> =
        fields.iter().map(|f| (&*f.name, f)).collect();

    let name_field = by_name["name"];
    assert_eq!(name_field.type_class, SchemaType::String);
    assert_eq!(&*name_field.owner_class, "User");
    assert_eq!(name_field.framework, FrameworkId::Pydantic);

    let age_field = by_name["age"];
    assert_eq!(age_field.type_class, SchemaType::Int);
    assert_eq!(&*age_field.owner_class, "User");
}

/// Optional / union type `str | None` — field is still emitted; type_class is
/// `Other` because `classify_python_type` receives the raw text `"str | None"`
/// which is not a single-token primary type.
#[test]
fn test_optional_union_type_emitted_as_other() {
    let src =
        "from pydantic import BaseModel\n\nclass User(BaseModel):\n    email: str | None = None\n";
    let fields = run(src, true);

    assert_eq!(fields.len(), 1);
    assert_eq!(
        fields[0].type_class,
        SchemaType::Other,
        "union type resolves to Other"
    );
    assert_eq!(&*fields[0].name, "email");
}

/// Generic type `list[str]` — field emitted, type_class is `Other`.
#[test]
fn test_generic_type_emitted_as_other() {
    let src =
        "from pydantic import BaseModel\n\nclass Tags(BaseModel):\n    items: list[str] = []\n";
    let fields = run(src, true);

    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].type_class, SchemaType::Other);
}

/// Import gate: file with no pydantic import → zero fields extracted.
#[test]
fn test_no_pydantic_import_zero_fields() {
    let src = "class User(BaseModel):\n    name: str\n    age: int\n";
    let fields = run(src, false);

    assert!(
        fields.is_empty(),
        "import gate must block emission without pydantic import"
    );
}

/// Plain class (no BaseModel superclass) with type annotations → zero fields.
/// The `(#eq? @_super "BaseModel")` predicate in the query prevents emission.
#[test]
fn test_plain_class_no_fields_emitted() {
    let src = "from pydantic import BaseModel\n\nclass Plain:\n    x: int = 0\n    y: str\n";
    let fields = run(src, true);

    assert!(
        fields.is_empty(),
        "plain class (no BaseModel) must produce no fields"
    );
}

/// Inherited model: `class Admin(User)` where `User(BaseModel)`.
///
/// Tree-sitter evaluates the `(#eq? @_super "BaseModel")` predicate on the
/// *literal* superclass identifier text. `Admin` extends `User`, not
/// `BaseModel` directly — so only `User`'s fields appear. `Admin.role` is
/// captured only if Admin also lists `BaseModel` as its direct base.
///
/// BlindSpot: cross-class inheritance resolution requires multi-file symbol
/// lookup beyond single-file tree-sitter scope. Inheritance chains where the
/// intermediate class is defined in another file (common in real codebases)
/// are not captured. This is a known, documented limitation — not a bug.
#[test]
#[ignore = "BlindSpot: inherited fields via intermediate class require cross-file symbol resolution beyond single-file tree-sitter scope"]
fn test_inherited_model_own_fields_only() {
    let src = "from pydantic import BaseModel\n\nclass User(BaseModel):\n    name: str\n\nclass Admin(User):\n    role: str\n";
    let fields = run(src, true);

    let owners: Vec<&str> = fields.iter().map(|f| &*f.owner_class).collect();
    assert!(
        !owners.contains(&"Admin"),
        "Admin is not a direct BaseModel subclass"
    );
    assert!(
        owners.contains(&"User"),
        "User(BaseModel) fields must be captured"
    );
}

/// Multi-class file: two independent BaseModel subclasses → fields from both.
#[test]
fn test_multiple_models_in_file() {
    let src = "from pydantic import BaseModel\n\nclass Foo(BaseModel):\n    x: int\n\nclass Bar(BaseModel):\n    y: str\n    z: bool\n";
    let fields = run(src, true);

    assert_eq!(fields.len(), 3, "Foo(1) + Bar(2) = 3 total fields");
    let owners: Vec<&str> = fields.iter().map(|f| &*f.owner_class).collect();
    assert!(owners.contains(&"Foo"));
    assert!(owners.contains(&"Bar"));
}

/// `parse_file` integration smoke test: the Python provider populates
/// `LocalGraph.schema_fields` when pydantic is imported.
#[test]
fn test_parse_file_populates_schema_fields() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "from pydantic import BaseModel\n\nclass Item(BaseModel):\n    name: str\n    price: float\n";
    let provider = PythonProvider::new().expect("provider init");
    let local = provider
        .parse_file("models.py".as_ref(), src.as_bytes())
        .expect("parse_file");

    let fields = local.schema_fields.expect("schema_fields must be Some");
    assert_eq!(fields.len(), 2, "Item has 2 fields");
    // Post-T4-7: owned `Box<str>` is directly readable.
    let names: Vec<&str> = fields.iter().map(|f| &*f.name).collect();
    assert!(names.contains(&"name"));
    assert!(names.contains(&"price"));
    assert!(fields.iter().all(|f| f.framework == FrameworkId::Pydantic));
    assert!(fields.iter().all(|f| &*f.owner_class == "Item"));
}

/// No pydantic import → `LocalGraph.schema_fields` is `None`.
#[test]
fn test_parse_file_no_import_schema_fields_none() {
    use ecp_analyzer::python::parser::PythonProvider;
    use ecp_core::analyzer::provider::LanguageProvider;

    let src = "class Item:\n    name: str\n    price: float\n";
    let provider = PythonProvider::new().expect("provider init");
    let local = provider
        .parse_file("plain.py".as_ref(), src.as_bytes())
        .expect("parse_file");

    assert!(
        local.schema_fields.is_none(),
        "no pydantic import → schema_fields must be None"
    );
}
