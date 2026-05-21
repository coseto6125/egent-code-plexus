//! Per-framework SchemaFieldConfig constants for Python frameworks.
//! Used by `python/parser.rs::parse_file` via the shared dispatcher
//! `crate::schema_field::extract_schema_fields`.

use crate::schema_field::{classify_python_type, SchemaFieldConfig};
use ecp_core::analyzer::types::FrameworkId;

pub const PYDANTIC_CONFIG: SchemaFieldConfig = SchemaFieldConfig {
    framework: FrameworkId::Pydantic,
    owner_capture: "pydantic.owner",
    name_capture: "pydantic.field",
    type_capture: "pydantic.type",
    import_gate: &["pydantic"],
    type_classifier: classify_python_type,
};
