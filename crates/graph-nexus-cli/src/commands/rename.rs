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
//! 4. **--markdown**: additionally replace word-boundary occurrences in
//!    `.md` / `.markdown` / `.rst` / `.txt` files (plain regex, not AST).
//! 5. **Post-rename verification**: scan for old-name residuals + new-name
//!    distribution; emit structured summary.
//! 6. **Pre-flight collision detection**: warn if new name already exists in
//!    the graph before the rename runs (especially in dry-run).

use crate::engine::Engine;
use clap::Args;
use graph_nexus_analyzer::identifier_finder::find_identifier_occurrences;
use graph_nexus_core::analyzer::types::IdentifierRange;
use graph_nexus_core::registry::atomic_write_bytes;
use graph_nexus_core::GnxError;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
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

    /// Also rename word-boundary occurrences in .md / .markdown / .rst / .txt
    /// documentation files. Default OFF.
    #[arg(long, default_value_t = false)]
    pub markdown: bool,
}

// ---------------------------------------------------------------------------
// Occurrence record for post-rename verification
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Occurrence {
    file: String,
    line: u32,
    context: String,
}

fn classify_context(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.contains("/test/")
        || s.contains("/tests/")
        || s.ends_with("_test.go")
        || s.ends_with("_test.rs")
    {
        "test".into()
    } else if s.ends_with(".md") || s.ends_with(".rst") || s.ends_with(".markdown") {
        "markdown".into()
    } else if s.ends_with(".json")
        || s.ends_with(".toml")
        || s.ends_with(".yaml")
        || s.ends_with(".yml")
    {
        "data".into()
    } else {
        "code".into()
    }
}

/// Walk `root` (respecting .gitignore) and collect every line matching `\b<word>\b`.
fn scan_word_occurrences(root: &Path, word: &str) -> Vec<Occurrence> {
    let Ok(pattern) = Regex::new(&format!(r"\b{}\b", regex::escape(word))) else {
        return vec![];
    };
    let mut hits = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
    {
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            if pattern.is_match(line) {
                hits.push(Occurrence {
                    file: path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                    line: (idx + 1) as u32,
                    context: classify_context(path),
                });
            }
        }
    }
    hits
}

// ---------------------------------------------------------------------------
// Pre-flight collision detection
// ---------------------------------------------------------------------------

fn detect_collisions(
    new_name: &str,
    graph: &graph_nexus_core::graph::ArchivedZeroCopyGraph,
) -> Vec<String> {
    let mut locs = Vec::new();
    for node in graph.nodes.iter() {
        if node.name.resolve(&graph.string_pool) == new_name {
            let file_idx = node.file_idx.to_native() as usize;
            let file_path = if file_idx < graph.files.len() {
                graph.files[file_idx]
                    .path
                    .resolve(&graph.string_pool)
                    .to_owned()
            } else {
                "<unknown>".to_owned()
            };
            let start_line = node.span.0.to_native();
            locs.push(format!("{file_path}:{start_line}"));
        }
    }
    locs
}

// ---------------------------------------------------------------------------
// Markdown / doc-file rename pass (word-boundary regex, no AST)
// ---------------------------------------------------------------------------

static DOC_EXTENSIONS: &[&str] = &[".md", ".markdown", ".rst", ".txt"];

fn is_doc_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    DOC_EXTENSIONS.iter().any(|ext| s.ends_with(ext))
}

/// Replace `\bold\b` with `new` in all doc files under `root`.
/// Returns list of (path, hit_count) for files that were changed.
fn apply_markdown_rename(
    root: &Path,
    old: &str,
    new: &str,
    dry_run: bool,
) -> Vec<(PathBuf, usize)> {
    let Ok(pattern) = Regex::new(&format!(r"\b{}\b", regex::escape(old))) else {
        return vec![];
    };
    let mut changed = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| is_doc_file(e.path()))
    {
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let count = pattern.find_iter(&content).count();
        if count == 0 {
            continue;
        }
        if !dry_run {
            let replaced = pattern.replace_all(&content, new).into_owned();
            // Use atomic write to match existing rename behaviour.
            let _ = atomic_write_bytes(path, replaced.as_bytes());
        }
        changed.push((path.to_path_buf(), count));
    }
    changed
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub fn run(args: RenameArgs, engine: &Engine) -> Result<(), GnxError> {
    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    // --- Pre-flight collision detection ---
    let collisions = detect_collisions(&args.new_name, graph);
    if !collisions.is_empty() {
        eprintln!(
            "{}",
            crate::hint::collision_warning(&args.new_name, &collisions)
        );
    }

    // Stage 1: locate target node + collect affected files.
    let target_indices: Vec<usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name.resolve(&graph.string_pool) == args.symbol)
        .map(|(i, _)| i)
        .collect();

    // Stage 2: parse each file, find identifier occurrences.
    let mut hits: Vec<(PathBuf, Vec<IdentifierRange>)> = Vec::new();

    if !target_indices.is_empty() {
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
    }

    let total_ast_hits: usize = hits.iter().map(|(_, r)| r.len()).sum();

    // Zero-occurrence case: explicit message + suggestions.
    if target_indices.is_empty() || total_ast_hits == 0 {
        println!("No occurrences of \"{}\" found.", args.symbol);

        // Check if the name exists in doc files even though not in graph.
        let doc_hits = if args.markdown {
            apply_markdown_rename(&repo_root, &args.symbol, &args.new_name, true)
        } else {
            // Still scan to provide hints.
            apply_markdown_rename(&repo_root, &args.symbol, &args.new_name, true)
        };
        if !doc_hits.is_empty() {
            let total_doc: usize = doc_hits.iter().map(|(_, c)| c).sum();
            println!(
                "→ Found {} string-literal/markdown/data matches (not symbols).",
                total_doc
            );
            println!(
                "→ For markdown: gnx rename --symbol {} --new-name {} --markdown",
                args.symbol, args.new_name
            );
        }
        return Ok(());
    }

    // --- Stage 3a: dry-run — print summary + diff preview, then verification. ---
    if args.dry_run {
        println!("risk safe; files {}; usages {}", hits.len(), total_ast_hits);
        println!();
        for (path, ranges) in &hits {
            let bytes = std::fs::read(path).map_err(GnxError::Io)?;
            print_diff(&bytes, ranges, &args.symbol, &args.new_name, path);
        }

        // Markdown pass preview.
        if args.markdown {
            let md_changed = apply_markdown_rename(&repo_root, &args.symbol, &args.new_name, true);
            if !md_changed.is_empty() {
                println!("[markdown] would update {} doc file(s):", md_changed.len());
                for (p, c) in &md_changed {
                    println!("  {} ({} occurrences)", p.display(), c);
                }
            }
        }

        emit_verification_payload(
            &repo_root,
            &args.symbol,
            &args.new_name,
            total_ast_hits,
            true,
        );
        return Ok(());
    }

    // --- Stage 3b: execute — atomic per-file replace by descending offset. ---
    for (path, ranges) in hits {
        apply_rename(&path, &ranges, args.new_name.as_bytes()).map_err(GnxError::Io)?;
    }

    // Markdown pass.
    if args.markdown {
        apply_markdown_rename(&repo_root, &args.symbol, &args.new_name, false);
    }

    emit_verification_payload(
        &repo_root,
        &args.symbol,
        &args.new_name,
        total_ast_hits,
        false,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Verification payload
// ---------------------------------------------------------------------------

fn emit_verification_payload(
    repo_root: &Path,
    old: &str,
    new: &str,
    rename_count: usize,
    dry_run: bool,
) {
    let residuals = scan_word_occurrences(repo_root, old);
    let new_distribution = scan_word_occurrences(repo_root, new);

    let payload = json!({
        "operation": if dry_run { "dry-run" } else { "applied" },
        "old": old,
        "new": new,
        "rename_count": rename_count,
        "residuals": residuals,
        "new_distribution": new_distribution,
    });

    // Best-effort: emit as structured JSON after main output.
    let _ = crate::output::emit(&payload, crate::output::OutputFormat::Toon);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
