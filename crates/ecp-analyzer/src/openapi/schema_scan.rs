//! OpenAPI 3.x / Swagger 2.0 schema-field extraction (T4-6).
//!
//! Lifts `components.schemas.<Name>.properties.<field>` (OpenAPI 3.x) and
//! `definitions.<Name>.properties.<field>` (Swagger 2.0) into
//! `RawSchemaField` nodes.
//!
//! **Out of scope (v1):** inline schemas under `paths.*` (request bodies,
//! response content, parameter schemas).  A future `--include-inline` flag
//! should cover those.  TODO: implement `--include-inline` to scan
//! `paths.<path>.<method>.{requestBody,responses,parameters}` schema trees.
//!
//! ## Pipeline gate
//! The provider first inspects the first 200 bytes for `openapi:` or
//! `swagger:` at column 0.  Non-OpenAPI YAML (k8s manifests, CI configs, Helm
//! values) is rejected before any parse work — zero serde overhead.

use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::{FrameworkId, LocalGraph, RawSchemaField, SchemaType};
use serde_json::Value;
use std::path::Path;

pub struct OpenApiProvider;

impl OpenApiProvider {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }
}

impl LanguageProvider for OpenApiProvider {
    fn name(&self) -> &'static str {
        "openapi"
    }

    fn parse_file(&self, path: &Path, source: &[u8]) -> anyhow::Result<LocalGraph> {
        // ── 200-byte prefix gate ──────────────────────────────────────────────
        // Reject files that don't start with `openapi:` or `swagger:` at
        // column 0 within the first 200 bytes.  This keeps k8s manifests, CI
        // configs, Helm values, and arbitrary YAML at zero serde cost.
        let probe = &source[..source.len().min(200)];
        if !has_openapi_marker(probe) {
            return Ok(LocalGraph {
                file_path: path.to_path_buf(),
                ..Default::default()
            });
        }

        let fields = extract_fields(path, source)?;
        let schema_fields = (!fields.is_empty()).then(|| fields.into_boxed_slice());

        Ok(LocalGraph {
            file_path: path.to_path_buf(),
            schema_fields,
            ..Default::default()
        })
    }
}

/// Returns `true` when `probe` (first ≤200 bytes) signals an OpenAPI or
/// Swagger document.
///
/// Two formats are detected:
/// - **YAML**: `openapi:` or `swagger:` at column 0 (start of file or after `\n`).
/// - **JSON**: `"openapi"` or `"swagger"` as the first string key in the object
///   (whitespace-tolerant, appears within the first 200 bytes).
///
/// Byte-level — runs before UTF-8 validation, no allocation, no parse.
/// `pub` so the YAML provider can gate OpenAPI extraction without re-implementing
/// the check.
pub fn has_openapi_marker(probe: &[u8]) -> bool {
    // ── YAML form: `openapi:` or `swagger:` at column 0 ─────────────────────
    if probe.starts_with(b"openapi:") || probe.starts_with(b"swagger:") {
        return true;
    }
    // Window size 9 = 1 (newline) + 8 (len of "swagger:").
    for window in probe.windows(9) {
        if window[0] == b'\n'
            && (window[1..].starts_with(b"openapi:") || window[1..].starts_with(b"swagger:"))
        {
            return true;
        }
    }

    // ── JSON form: `"openapi"` or `"swagger"` anywhere in the probe ──────────
    // JSON spec files start with `{` and contain `"openapi": "3.x"` near the
    // top. We look for the literal byte sequences; false positives in other JSON
    // are vanishingly rare (and the serde parse that follows will simply return
    // None from `resolve_schemas`).
    let probe_str = probe; // already &[u8]
    contains_subsequence(probe_str, b"\"openapi\"")
        || contains_subsequence(probe_str, b"\"swagger\"")
}

/// Naive byte-subsequence search (Boyer-Moore not needed at 200-byte scale).
fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Dispatch on file extension to parse as YAML or JSON, then walk schema tree.
/// `pub` so `YamlProvider` can call this after confirming the OpenAPI marker.
pub fn extract_fields(path: &Path, source: &[u8]) -> anyhow::Result<Vec<RawSchemaField>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let root: Value = match ext.as_str() {
        "json" => serde_json::from_slice(source)
            .map_err(|e| anyhow::anyhow!("openapi: JSON parse error in {:?}: {}", path, e))?,
        // Default (yml / yaml / unknown extension): attempt YAML parse.
        _ => {
            let text = std::str::from_utf8(source)
                .map_err(|e| anyhow::anyhow!("openapi: UTF-8 decode error in {:?}: {}", path, e))?;
            serde_yaml::from_str(text)
                .map_err(|e| anyhow::anyhow!("openapi: YAML parse error in {:?}: {}", path, e))?
        }
    };

    let (framework, schemas_map) = resolve_schemas(&root);
    let Some(schemas_map) = schemas_map else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (schema_name, schema_def) in schemas_map {
        let Some(props) = schema_def.get("properties").and_then(Value::as_object) else {
            continue;
        };
        for (field_name, field_def) in props {
            let type_class = classify_openapi_type(field_def);
            // Span (0,0,0,0) — serde_json/serde_yaml Values carry no byte
            // offsets; sentinel consistent with SQL and other non-tree-sitter
            // extractors.
            out.push(RawSchemaField {
                name: field_name.as_str().into(),
                type_class,
                owner_class: schema_name.as_str().into(),
                framework,
                span: (0, 0, 0, 0),
            });
        }
    }

    Ok(out)
}

/// Return `(FrameworkId, Option<schema map>)` for the document.
///
/// - `swagger: "2.x"` → `(Swagger, root["definitions"])`
/// - `openapi: "3.x"` → `(OpenApi, root["components"]["schemas"])`
/// - Unrecognised → `(OpenApi, root["components"]["schemas"])` (best-effort)
fn resolve_schemas(root: &Value) -> (FrameworkId, Option<&serde_json::Map<String, Value>>) {
    if root
        .get("swagger")
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with('2'))
    {
        return (
            FrameworkId::Swagger,
            root.get("definitions").and_then(Value::as_object),
        );
    }
    let schemas = root
        .get("components")
        .and_then(|c| c.get("schemas"))
        .and_then(Value::as_object);
    (FrameworkId::OpenApi, schemas)
}

/// Classify an OpenAPI field definition object into a `SchemaType`.
///
/// Mapping per roadmap T4-6 + OQ-7:
/// - `type: string`, `format: date-time` → `Datetime`
/// - `type: string`                       → `String`
/// - `type: integer`                      → `Int`
/// - `type: number`                       → `Float`
/// - `type: boolean`                      → `Bool`
/// - `type: object` / `type: array`       → `Json`
/// - `$ref` / allOf / anyOf / missing     → `Other`
fn classify_openapi_type(field_def: &Value) -> SchemaType {
    let type_str = field_def.get("type").and_then(Value::as_str).unwrap_or("");
    let format_str = field_def
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("");

    match type_str {
        "string" if format_str == "date-time" => SchemaType::Datetime,
        "string" => SchemaType::String,
        "integer" => SchemaType::Int,
        "number" => SchemaType::Float,
        "boolean" => SchemaType::Bool,
        "object" | "array" => SchemaType::Json,
        _ => SchemaType::Other,
    }
}
