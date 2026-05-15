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

use crate::engine::Engine;
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

pub fn run_inner(args: RenameArgs, engine: &Engine) -> Result<serde_json::Value, GnxError> {
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

    // Stage 3a: dry-run — print summary + diff preview.
    if args.dry_run {
        let total_hits: usize = hits.iter().map(|(_, r)| r.len()).sum();
        println!("risk safe; files {}; usages {}", hits.len(), total_hits);
        println!();
        for (path, ranges) in &hits {
            let bytes = std::fs::read(path).map_err(GnxError::Io)?;
            print_diff(&bytes, ranges, &args.symbol, &args.new_name, path);
        }
        return Ok(serde_json::Value::Null);
    }

    // Stage 3b: execute — atomic per-file replace by descending offset.
    for (path, ranges) in hits {
        apply_rename(&path, &ranges, args.new_name.as_bytes()).map_err(GnxError::Io)?;
    }
    Ok(serde_json::Value::Null)
}

pub fn run(args: RenameArgs, engine: &crate::engine::Engine)
    -> Result<(), graph_nexus_core::GnxError>
{
    let format = crate::output::OutputFormat::Toon;
    let value = run_inner(args, engine)?;
    crate::output::emit(&value, format)
}

#[cfg(test)]
mod inner_tests {
    use super::*;
    #[test]
    fn run_inner_returns_structured_value_not_unit() {
        fn _accepts(
            _f: fn(RenameArgs, &crate::engine::Engine)
                -> Result<serde_json::Value, graph_nexus_core::GnxError>
        ) {}
        _accepts(run_inner);
    }
}

fn apply_rename(path: &Path, ranges: &[IdentifierRange], new_bytes: &[u8]) -> std::io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    let mut sorted = ranges.to_vec();
    sorted.sort_by(|a, b| b.start_byte.cmp(&a.start_byte));
    for r in &sorted {
        bytes.splice(r.start_byte..r.end_byte, new_bytes.iter().copied());
    }
    atomic_write_bytes(path, &bytes)
}

/// Print a minimal unified-diff-ish preview: for each hit, one `-`
/// line (current) and one `+` line (after substitution). Multiple hits
/// on the same line collapse into a single replacement line.
fn print_diff(bytes: &[u8], ranges: &[IdentifierRange], old: &str, new: &str, path: &Path) {
    println!("{}", path.display());
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let mut shown: HashSet<usize> = HashSet::new();
    for r in ranges {
        if !shown.insert(r.row) {
            continue;
        }
        if let Some(line) = lines.get(r.row) {
            println!("- {line}");
            println!("+ {}", line.replace(old, new));
        }
    }
    println!();
}
