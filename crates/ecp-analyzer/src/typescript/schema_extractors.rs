//! Per-framework SchemaFieldConfig constants for TypeScript.
//! Used by `typescript/parser.rs::parse_file` via the shared dispatcher.

use crate::schema_field::{classify_ts_type, SchemaFieldConfig};
use ecp_core::analyzer::types::FrameworkId;

/// TypeScript `interface` property extraction config.
///
/// Fires on every `interface_declaration` regardless of imports — `interface`
/// is a TS language built-in requiring no framework import.  Empty
/// `import_gate` is treated as vacuously satisfied by `extract_schema_fields`.
pub const TS_INTERFACE_CONFIG: SchemaFieldConfig = SchemaFieldConfig {
    framework: FrameworkId::TypeScriptInterface,
    owner_capture: "ts.owner",
    name_capture: "ts.field",
    type_capture: "ts.type",
    import_gate: &[],
    type_classifier: classify_ts_type,
};
