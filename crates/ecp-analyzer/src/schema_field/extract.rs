use super::config::SchemaFieldConfig;
use crate::framework_helpers::has_import_from;
use ecp_core::analyzer::types::{RawImport, RawSchemaField, SchemaType};
use ecp_core::pool::StringPool;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor, Tree};

/// Walk all captures produced by `query` against `tree`/`source`, dispatch
/// each match to the first `SchemaFieldConfig` whose import-gate is satisfied,
/// and emit a `RawSchemaField` for every accepted match.
///
/// `pool` is used to intern `name` and `owner_class` strings.  The caller
/// typically passes the same pool used for the rest of the file's `LocalGraph`
/// to maximise dedup across nodes.
///
/// The caller is responsible for supplying a query whose capture names align
/// with the `owner_capture`, `name_capture`, and `type_capture` fields of at
/// least one config.  Captures not referenced by any config are silently
/// ignored — forward-compatible with queries that carry extra context for
/// T4-2..T4-6 frameworks.
///
/// # Import-gate semantics
/// A config fires only when `has_import_from(imports, config.import_gate)`
/// returns `true`.  When no config's gate is satisfied by the file's imports,
/// this function returns an empty `Vec` — no false positives.
pub fn extract_schema_fields(
    tree: &Tree,
    source: &[u8],
    query: &Query,
    configs: &[SchemaFieldConfig],
    imports: &[RawImport],
    pool: &mut StringPool,
) -> Vec<RawSchemaField> {
    // Identify which configs are live for this file once, not per-match.
    let active: Vec<&SchemaFieldConfig> = configs
        .iter()
        .filter(|c| has_import_from(imports, c.import_gate))
        .collect();

    if active.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        // Try each active config in declaration order; first match wins.
        'configs: for config in &active {
            let mut owner_text: Option<&str> = None;
            let mut name_text: Option<&str> = None;
            let mut type_text: Option<&str> = None;

            for cap in m.captures {
                let cap_name = &query.capture_names()[cap.index as usize];
                let node_text = cap.node.utf8_text(source).unwrap_or("");
                if *cap_name == config.owner_capture {
                    owner_text = Some(node_text);
                } else if *cap_name == config.name_capture {
                    name_text = Some(node_text);
                } else if *cap_name == config.type_capture {
                    type_text = Some(node_text);
                }
            }

            let (Some(owner), Some(name)) = (owner_text, name_text) else {
                continue 'configs;
            };

            let type_class = (config.type_classifier)(type_text.unwrap_or(""));

            let start = m.captures[0].node.start_position();
            let end = m.captures[0].node.end_position();
            let span = (
                start.row as u32,
                start.column as u32,
                end.row as u32,
                end.column as u32,
            );

            out.push(RawSchemaField {
                name: pool.add(name),
                type_class,
                owner_class: pool.add(owner),
                framework: String::from(config.framework),
                span,
            });
            break 'configs;
        }
    }

    out
}

/// Canonical type classifier for Python / SQLAlchemy naming conventions.
/// T4-2..T4-6 configs may supply their own function pointer for
/// language-specific type mappings.
pub fn classify_python_type(raw: &str) -> SchemaType {
    match raw {
        "str" | "String" | "Text" | "VARCHAR" | "CHAR" | "NVARCHAR" | "TEXT" => SchemaType::String,
        "int" | "Integer" | "BigInteger" | "SmallInteger" | "INT" | "BIGINT" | "SMALLINT" => {
            SchemaType::Int
        }
        "float" | "Float" | "Numeric" | "FLOAT" | "DECIMAL" | "NUMERIC" | "REAL" => {
            SchemaType::Float
        }
        "bool" | "Boolean" | "BOOLEAN" => SchemaType::Bool,
        "datetime" | "DateTime" | "DATETIME" | "TIMESTAMP" | "date" | "Date" | "time" | "Time" => {
            SchemaType::Datetime
        }
        "dict" | "JSON" | "JSONB" | "Json" => SchemaType::Json,
        _ => SchemaType::Other,
    }
}

/// Canonical type classifier for TypeScript / JavaScript primitive types.
pub fn classify_ts_type(raw: &str) -> SchemaType {
    match raw {
        "string" | "String" => SchemaType::String,
        "number" | "Number" | "bigint" | "BigInt" => SchemaType::Int,
        "boolean" | "Boolean" => SchemaType::Bool,
        "Date" => SchemaType::Datetime,
        "object" | "Record" | "Map" => SchemaType::Json,
        _ => SchemaType::Other,
    }
}
