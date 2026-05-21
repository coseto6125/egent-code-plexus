use ecp_core::analyzer::types::SchemaType;

/// Table-driven configuration for a single ORM / schema framework.
///
/// Mirrors ref-gitnexus `field-extractors/generic.ts` `FieldExtractionConfig`
/// (192 L): instead of N separate hardcoded detectors, each framework ships
/// a `SchemaFieldConfig` constant and the shared `extract_schema_fields`
/// dispatch loop handles all of them uniformly.
///
/// `&'static str` fields are intentional — configs are `const` items, so all
/// strings live in the binary's read-only segment (zero heap alloc). These
/// structs are **never** archived by rkyv (only `RawSchemaField` is, and it
/// stores `framework` as `String` to satisfy rkyv's `Archive` bound). The
/// extractor converts `config.framework` to `String::from(...)` once per
/// emitted `RawSchemaField`.
pub struct SchemaFieldConfig {
    /// Human-readable label written into `RawSchemaField::framework`.
    /// Example: `"pydantic"`, `"sqlalchemy"`, `"typescript-interface"`.
    pub framework: &'static str,
    /// Tree-sitter capture name identifying the owner class / struct.
    /// Example: `"owner"`, `"class_name"`.
    pub owner_capture: &'static str,
    /// Tree-sitter capture name identifying the field / column name.
    pub name_capture: &'static str,
    /// Tree-sitter capture name identifying the type annotation text.
    /// May be an empty string when the query does not capture a type;
    /// `extract_schema_fields` treats an empty capture text as `SchemaType::Other`.
    pub type_capture: &'static str,
    /// Import-gate: at least one entry must match a `RawImport::source` for
    /// this config to fire.  Checked via `framework_helpers::has_import_from`.
    pub import_gate: &'static [&'static str],
    /// Maps raw type-annotation text (e.g. `"str"`, `"int"`, `"DateTime"`)
    /// to the canonical `SchemaType` variant.
    pub type_classifier: fn(&str) -> SchemaType,
}
