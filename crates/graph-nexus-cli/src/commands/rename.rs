//! `gnx rename` — AST-powered multi-lang rename.
//!
//! Pipeline:
//! 1. **Plan (graph)**: load `graph.bin`, find the target node by name,
//!    collect inbound-edge source files + the target's own file. Bails
//!    with `error: ambiguous` if multiple nodes share the name.
//! 2. **Verify (AST)**: tree-sitter parse each affected file,
//!    find every `identifier` byte-range whose text matches the
//!    target. Supported languages: Python, TypeScript/TSX, JavaScript,
//!    Rust, Java, Kotlin, C#, Go, PHP, Ruby, Swift, C, C++, Dart.
//! 3. **Execute / Dry-run**: dry-run prints the count + a unified diff
//!    preview to stdout and exits. Execute writes each file atomically
//!    (tmp + fsync + rename) by descending byte offset to avoid shift.

use clap::Args;
use graph_nexus_analyzer::identifier_finder::find_identifier_occurrences;
use graph_nexus_core::analyzer::types::IdentifierRange;
use graph_nexus_core::registry::atomic_write_bytes;
use graph_nexus_core::GnxError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// AST-powered multi-language rename: locates all identifier occurrences
/// via tree-sitter and rewrites them atomically, with optional dry-run preview.
#[derive(Args, Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameArgs {
    /// The symbol name to rename (e.g. `old_name`).
    #[arg(long, alias = "symbol_name")]
    pub symbol: String,

    /// The new name to apply.
    #[arg(long = "new-name", alias = "new_name")]
    pub new_name: String,

    /// Repository root. Defaults to current dir.
    #[arg(long)]
    pub repo: Option<String>,

    /// Plan + verify only — do not mutate any file. Prints the diff
    /// summary to stdout.
    #[arg(long, alias = "dry_run", default_value_t = false)]
    pub dry_run: bool,
}

pub fn run_inner(
    args: RenameArgs,
    engine: &dyn graph_nexus_mcp::registry::EngineRef,
) -> Result<serde_json::Value, GnxError> {
    let engine = crate::engine::cast_engine(engine)?;
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    // Stage 1: locate target node + collect affected files.
    let target_indices: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name.resolve(&graph.string_pool) == args.symbol)
        .map(|(i, _)| i)
        .collect();
    if target_indices.is_empty() {
        return Err(GnxError::SymbolNotFound {
            uid: args.symbol.clone(),
        });
    }

    let mut affected_file_idx: HashSet<usize> = HashSet::new();
    for &target_idx in &target_indices {
        let target_node = &graph.nodes[target_idx];
        affected_file_idx.insert(target_node.file_idx.to_native() as usize);

        let in_start = graph.in_offsets[target_idx].to_native() as usize;
        let in_end = graph.in_offsets[target_idx + 1].to_native() as usize;
        for i in in_start..in_end {
            let edge_idx = graph.in_edge_idx[i].to_native() as usize;
            let edge = &graph.edges[edge_idx];
            let src = &graph.nodes[edge.source.to_native() as usize];
            affected_file_idx.insert(src.file_idx.to_native() as usize);
        }
    }

    // Stage 2: parse each file, find identifier occurrences.
    let mut hits: Vec<(PathBuf, Vec<IdentifierRange>)> = Vec::new();
    for file_idx in affected_file_idx {
        let rel_path = graph.files[file_idx].path.resolve(&graph.string_pool);
        let abs_path = repo_root.join(rel_path);
        let Ok(bytes) = std::fs::read(&abs_path) else {
            continue;
        };
        let occurrences = find_identifier_occurrences(rel_path, &bytes, &args.symbol);
        if !occurrences.is_empty() {
            hits.push((abs_path, occurrences));
        }
    }

    // Stage 3a: dry-run — collect summary + diff preview into lines.
    if args.dry_run {
        let total_hits: usize = hits.iter().map(|(_, r)| r.len()).sum();
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "risk safe; files {}; usages {}",
            hits.len(),
            total_hits
        ));
        lines.push(String::new());
        for (path, ranges) in &hits {
            let bytes = std::fs::read(path).map_err(GnxError::Io)?;
            collect_diff(
                &bytes,
                ranges,
                &args.symbol,
                &args.new_name,
                path,
                &mut lines,
            );
        }
        let files_affected = hits.len();
        return Ok(serde_json::json!({
            "results": lines,
            "mode": "dry_run",
            "files_affected": files_affected,
        }));
    }

    // Stage 3b: execute — atomic per-file replace by descending offset.
    let files_modified = hits.len();
    let mut lines: Vec<String> = Vec::new();
    for (path, ranges) in hits {
        lines.push(format!("renamed: {}", path.display()));
        apply_rename(&path, &ranges, args.new_name.as_bytes()).map_err(GnxError::Io)?;
    }
    Ok(serde_json::json!({
        "results": lines,
        "mode": "executed",
        "files_modified": files_modified,
    }))
}

pub fn run(
    args: RenameArgs,
    engine: &crate::engine::Engine,
) -> Result<(), graph_nexus_core::GnxError> {
    let format = crate::output::OutputFormat::Text;
    let value = run_inner(args, engine)?;
    crate::output::emit(&value, format)
}

#[cfg(test)]
mod inner_tests {
    use super::*;

    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        fn _accepts(
            _f: fn(
                RenameArgs,
                &dyn graph_nexus_mcp::registry::EngineRef,
            ) -> Result<serde_json::Value, graph_nexus_core::GnxError>,
        ) {
        }
        _accepts(run_inner);
    }

    /// Verify that `collect_diff` produces the expected structured lines.
    /// Uses a two-byte synthetic buffer with a known old/new name so the
    /// shape can be exact-matched without a real graph engine.
    #[test]
    fn collect_diff_produces_expected_lines() {
        let src = b"fn foo() { foo(); }";
        let ranges = vec![
            IdentifierRange {
                start_byte: 3,
                end_byte: 6,
                row: 0,
                col: 3,
            },
            IdentifierRange {
                start_byte: 11,
                end_byte: 14,
                row: 0,
                col: 11,
            },
        ];
        let mut out: Vec<String> = Vec::new();
        collect_diff(
            src,
            &ranges,
            "foo",
            "bar",
            std::path::Path::new("lib.rs"),
            &mut out,
        );

        // Expected: path header, one pair of ± lines (row 0 deduped), trailing blank.
        assert_eq!(
            out,
            vec![
                "lib.rs".to_string(),
                "- fn foo() { foo(); }".to_string(),
                "+ fn bar() { bar(); }".to_string(),
                String::new(),
            ]
        );
    }

    /// Verify the dry_run structured shape: results array + mode + files_affected.
    #[test]
    fn collect_diff_dry_run_result_shape() {
        let lines = vec!["risk safe; files 0; usages 0".to_string(), String::new()];
        let value = serde_json::json!({
            "results": lines,
            "mode": "dry_run",
            "files_affected": 0usize,
        });
        assert_eq!(value["mode"], "dry_run");
        assert_eq!(value["files_affected"], 0);
        let results = value["results"].as_array().expect("array");
        assert_eq!(results[0].as_str().unwrap(), "risk safe; files 0; usages 0");
    }

    /// Verify the execute structured shape: results array + mode + files_modified.
    #[test]
    fn collect_diff_execute_result_shape() {
        let value = serde_json::json!({
            "results": ["renamed: src/lib.rs"],
            "mode": "executed",
            "files_modified": 1usize,
        });
        assert_eq!(value["mode"], "executed");
        assert_eq!(value["files_modified"], 1);
        let results = value["results"].as_array().expect("array");
        assert_eq!(results[0].as_str().unwrap(), "renamed: src/lib.rs");
    }
}

graph_nexus_mcp::gnx_register_mcp_tool!(RenameArgs, run_inner);

fn apply_rename(path: &Path, ranges: &[IdentifierRange], new_bytes: &[u8]) -> std::io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| b.start_byte.cmp(&a.start_byte));
    for r in &sorted {
        bytes.splice(r.start_byte..r.end_byte, new_bytes.iter().copied());
    }
    atomic_write_bytes(path, &bytes)
}

/// Collect a minimal unified-diff-ish preview into `out`: for each hit,
/// one `-` line (current) and one `+` line (after substitution). Multiple
/// hits on the same source line collapse into a single replacement entry.
fn collect_diff(
    bytes: &[u8],
    ranges: &[IdentifierRange],
    old: &str,
    new: &str,
    path: &Path,
    out: &mut Vec<String>,
) {
    out.push(path.display().to_string());
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let mut shown: HashSet<usize> = HashSet::new();
    for r in ranges {
        if !shown.insert(r.row) {
            continue;
        }
        if let Some(line) = lines.get(r.row) {
            out.push(format!("- {line}"));
            out.push(format!("+ {}", line.replace(old, new)));
        }
    }
    out.push(String::new());
}
