use super::config::SchemaFieldConfig;
use crate::framework_helpers::has_import_from;
use ecp_core::analyzer::types::{RawImport, RawSchemaField, SchemaType};
use ecp_core::pool::StringPool;
use rustc_hash::FxHashMap;
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
    // Empty import_gate is vacuously satisfied — language built-ins (e.g.
    // TypeScript `interface`) require no import to carry schema semantics.
    let active: Vec<&SchemaFieldConfig> = configs
        .iter()
        .filter(|c| c.import_gate.is_empty() || has_import_from(imports, c.import_gate))
        .collect();

    if active.is_empty() {
        return Vec::new();
    }

    // Pre-build capture-name → [(active_idx, role)] map once per call.
    // Multiple active configs may share the same capture name (e.g. both
    // Pydantic and SQLAlchemy name their owner capture "owner"), so the value
    // is a Vec. Done once here; amortises across all (match × capture) pairs
    // in the file, keeping the per-capture cost O(1) instead of O(K).
    let mut cap_map: FxHashMap<&str, Vec<(usize, CaptureRole)>> =
        FxHashMap::with_capacity_and_hasher(active.len() * 3, Default::default());
    for (idx, cfg) in active.iter().enumerate() {
        cap_map
            .entry(cfg.owner_capture)
            .or_default()
            .push((idx, CaptureRole::Owner));
        cap_map
            .entry(cfg.name_capture)
            .or_default()
            .push((idx, CaptureRole::Name));
        // type_capture may be empty string when the query omits a type node;
        // skip to avoid a catch-all "" entry that matches every capture.
        if !cfg.type_capture.is_empty() {
            cap_map
                .entry(cfg.type_capture)
                .or_default()
                .push((idx, CaptureRole::Type));
        }
    }

    let n_active = active.len();
    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    // Reusable per-match slot table: [owner, name, type] text per active config.
    // Allocated once, cleared before each match.
    let mut slots: Vec<[Option<&str>; 3]> = vec![[None; 3]; n_active];

    // Hoist `capture_names()` out of the match × capture nested loop —
    // tree_sitter's `capture_names()` re-walks the query's name table on
    // each call, and the inner loop touches O(matches × captures_per_match)
    // captures across 25k+ files in cold-index path.
    let cap_names = query.capture_names();

    while let Some(m) = matches.next() {
        // Reset slots for this match.
        for s in slots.iter_mut() {
            *s = [None; 3];
        }

        // Single O(M) pass over captures; O(1) lookup per capture.
        for cap in m.captures {
            let cap_name = cap_names[cap.index as usize];
            if let Some(entries) = cap_map.get(cap_name) {
                let node_text = cap.node.utf8_text(source).unwrap_or("");
                for &(idx, role) in entries {
                    slots[idx][role as usize] = Some(node_text);
                }
            }
        }

        // Scan active configs in declaration order; first fully-populated wins.
        for (idx, config) in active.iter().enumerate() {
            let [owner_opt, name_opt, type_opt] = slots[idx];
            let (Some(owner), Some(name)) = (owner_opt, name_opt) else {
                continue;
            };

            let type_class = (config.type_classifier)(type_opt.unwrap_or(""));

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
                framework: config.framework,
                span,
            });
            break;
        }
    }

    out
}

/// Capture role for the dispatch map — maps a slot index to its semantic
/// meaning within `SchemaFieldConfig`.
#[derive(Clone, Copy)]
enum CaptureRole {
    Owner = 0,
    Name = 1,
    Type = 2,
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
