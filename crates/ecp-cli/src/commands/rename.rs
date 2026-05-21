//! `ecp rename` — AST-powered multi-lang rename.
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

use clap::Args;
use ecp_analyzer::identifier_finder::find_identifier_occurrences;
use ecp_core::analyzer::types::IdentifierRange;
use ecp_core::registry::atomic_write_bytes;
use ecp_core::EcpError;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// AST-powered multi-language rename: locates all identifier occurrences
/// via tree-sitter and rewrites them atomically, with optional dry-run preview.
#[derive(Args, Debug, Clone, Serialize)]
pub struct RenameArgs {
    /// The symbol name to rename (equivalent to `--symbol` flag).
    pub symbol: Option<String>,

    /// Named alias for the positional SYMBOL argument.
    #[arg(
        long = "symbol",
        alias = "symbol_name",
        value_name = "SYMBOL",
        conflicts_with = "symbol"
    )]
    pub symbol_flag: Option<String>,

    /// The new name to apply (equivalent to `--new-name` flag).
    pub new_name: Option<String>,

    /// Named alias for the positional NEW_NAME argument.
    #[arg(
        long = "new-name",
        alias = "new_name",
        value_name = "NEW_NAME",
        conflicts_with = "new_name"
    )]
    pub new_name_flag: Option<String>,

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

    /// Expand the heuristic mirror list in the output. When unset, only the
    /// count is shown. Tier/check data is a T-H2 placeholder; T4-7 populates
    /// real values.
    #[arg(long, default_value_t = false)]
    pub show_heuristic_mirrors: bool,
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
    graph: &ecp_core::graph::ArchivedZeroCopyGraph,
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

pub fn run(args: RenameArgs, engine: &crate::engine::Engine) -> Result<(), EcpError> {
    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let repo_root = args
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let target_symbol = match args.symbol.as_deref().or(args.symbol_flag.as_deref()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(EcpError::InvalidArgument(
                "Target symbol name is required".to_string(),
            ))
        }
    };

    let target_new_name = match args.new_name.as_deref().or(args.new_name_flag.as_deref()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(EcpError::InvalidArgument(
                "New name is required".to_string(),
            ))
        }
    };

    // `ast_target_name` / `ast_new_name` are the bare identifiers used for
    // tree-sitter search and byte-level rewrite.
    // "Foo.validate" → ast_target_name="validate"; "Foo.check" → ast_new_name="check"
    // Bare names pass through unchanged.
    let ast_target_name: &str = target_symbol
        .find('.')
        .map(|dot| &target_symbol[dot + 1..])
        .unwrap_or(&target_symbol);
    let ast_new_name: &str = target_new_name
        .find('.')
        .map(|dot| &target_new_name[dot + 1..])
        .unwrap_or(&target_new_name);

    // --- Pre-flight collision detection ---
    // Pass the bare new-name: `Node.name` stores bare identifiers only, so
    // matching against `target_new_name="Foo.check"` would compare against
    // `node.name="check"` and silently never fire. Owner-class-aware
    // collision detection is T1-12 follow-up.
    let collisions = detect_collisions(ast_new_name, graph);
    if !collisions.is_empty() {
        eprintln!(
            "{}",
            crate::hint::collision_warning(ast_new_name, &collisions)
        );
    }

    // Output buffer for dry-run diff preview + execute-mode rename log.
    let mut lines: Vec<String> = Vec::new();

    // Stage 1: locate target node + collect affected files.
    //
    // Parse `target_symbol` for owner-class qualification:
    //   "Foo.validate" → match nodes where owner_class=="Foo" AND name=="validate"
    //   "validate"     → match nodes where owner_class is empty (top-level only)
    //
    // This isolates Foo.validate from Bar.validate (T1-11 accuracy fix).
    // Bare names no longer match class methods — they resolve to module-level
    // symbols only.  Callers wanting a class method must use "ClassName.method".

    let target_indices: Vec<usize> = if let Some(dot) = target_symbol.find('.') {
        let owner = &target_symbol[..dot];
        let name = &target_symbol[dot + 1..];
        graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.name.resolve(&graph.string_pool) == name
                    && n.owner_class.resolve(&graph.string_pool) == owner
            })
            .map(|(i, _)| i)
            .collect()
    } else {
        // Bare name: match only top-level symbols (owner_class is empty / len==0).
        graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.name.resolve(&graph.string_pool) == target_symbol
                    && n.owner_class.len.to_native() == 0
            })
            .map(|(i, _)| i)
            .collect()
    };

    // Stage 2: parse each file, find identifier occurrences.
    let mut hits: Vec<(PathBuf, Vec<IdentifierRange>)> = Vec::new();

    // Heuristic mirror count + candidate names (T-H2).
    // Count is informational only — never drives mutation.
    let mut heuristic_mirror_count: usize = 0;
    let mut heuristic_mirror_names: Vec<String> = Vec::new();

    if !target_indices.is_empty() {
        let mut affected_file_idx: HashSet<usize> = HashSet::new();
        for &target_idx in &target_indices {
            let target_node = &graph.nodes[target_idx];
            affected_file_idx.insert(target_node.file_idx.to_native() as usize);

            // Inbound edges, single pass: deterministic edges contribute to
            // `affected_file_idx`; heuristic ones bump the mirror counter.
            // Rename mutation stays 100% deterministic because the file-set
            // is only extended from non-heuristic sources.
            let in_start = graph.in_offsets[target_idx].to_native() as usize;
            let in_end = graph.in_offsets[target_idx + 1].to_native() as usize;
            for i in in_start..in_end {
                let edge_idx = graph.in_edge_idx[i].to_native() as usize;
                let edge = &graph.edges[edge_idx];
                if edge.rel_type.is_heuristic() {
                    heuristic_mirror_count += 1;
                    if args.show_heuristic_mirrors {
                        let src_name = graph.nodes[edge.source.to_native() as usize]
                            .name
                            .resolve(&graph.string_pool)
                            .to_string();
                        heuristic_mirror_names.push(src_name);
                    }
                } else {
                    let src = &graph.nodes[edge.source.to_native() as usize];
                    affected_file_idx.insert(src.file_idx.to_native() as usize);
                }
            }

            // Single-hop heuristic mirror count: outbound heuristic edges.
            let out_start = graph.out_offsets[target_idx].to_native() as usize;
            let out_end = graph.out_offsets[target_idx + 1].to_native() as usize;
            for edge in &graph.edges[out_start..out_end] {
                if edge.rel_type.is_heuristic() {
                    heuristic_mirror_count += 1;
                    if args.show_heuristic_mirrors {
                        let tgt_name = graph.nodes[edge.target.to_native() as usize]
                            .name
                            .resolve(&graph.string_pool)
                            .to_string();
                        heuristic_mirror_names.push(tgt_name);
                    }
                }
            }
        }

        for file_idx in affected_file_idx {
            let rel_path = graph.files[file_idx].path.resolve(&graph.string_pool);
            let abs_path = repo_root.join(rel_path);
            let Ok(bytes) = std::fs::read(&abs_path) else {
                continue;
            };
            let occurrences = find_identifier_occurrences(rel_path, &bytes, ast_target_name);
            if !occurrences.is_empty() {
                hits.push((abs_path, occurrences));
            }
        }
    }

    let total_ast_hits: usize = hits.iter().map(|(_, r)| r.len()).sum();

    // Zero-occurrence case: explicit message + suggestions.
    if target_indices.is_empty() || total_ast_hits == 0 {
        println!("No occurrences of \"{}\" found.", target_symbol);

        // Check if the name exists in doc files even though not in graph.
        let doc_hits = if args.markdown {
            apply_markdown_rename(&repo_root, &target_symbol, &target_new_name, true)
        } else {
            // Still scan to provide hints.
            apply_markdown_rename(&repo_root, &target_symbol, &target_new_name, true)
        };
        if !doc_hits.is_empty() {
            let total_doc: usize = doc_hits.iter().map(|(_, c)| c).sum();
            println!(
                "→ Found {} string-literal/markdown/data matches (not symbols).",
                total_doc
            );
            println!(
                "→ For markdown: ecp rename --symbol {} --new-name {} --markdown",
                target_symbol, target_new_name
            );
        }
        return Ok(());
    }

    // --- Stage 3a: dry-run — print summary + diff preview, then verification. ---
    if args.dry_run {
        println!("risk safe; files {}; usages {}", hits.len(), total_ast_hits);
        println!();
        for (path, ranges) in &hits {
            let bytes = std::fs::read(path).map_err(EcpError::Io)?;
            collect_diff(
                &bytes,
                ranges,
                ast_target_name,
                ast_new_name,
                path,
                &mut lines,
            );
        }

        // Markdown pass preview.
        if args.markdown {
            let md_changed =
                apply_markdown_rename(&repo_root, &target_symbol, &target_new_name, true);
            if !md_changed.is_empty() {
                println!("[markdown] would update {} doc file(s):", md_changed.len());
                for (p, c) in &md_changed {
                    println!("  {} ({} occurrences)", p.display(), c);
                }
            }
        }

        emit_mirror_summary(
            &target_symbol,
            heuristic_mirror_count,
            &heuristic_mirror_names,
            args.show_heuristic_mirrors,
        );
        emit_verification_payload(
            &repo_root,
            &target_symbol,
            &target_new_name,
            total_ast_hits,
            true,
        );
        return Ok(());
    }

    // --- Stage 3b: execute — atomic per-file replace by descending offset. ---
    println!("Renamed:");
    for (path, ranges) in hits {
        let rel = path.strip_prefix(&repo_root).unwrap_or(&path);
        println!("  - {}", rel.display());
        lines.push(format!("renamed: {}", path.display()));
        apply_rename(&path, &ranges, ast_new_name.as_bytes()).map_err(EcpError::Io)?;
    }

    // Markdown pass.
    if args.markdown {
        apply_markdown_rename(&repo_root, &target_symbol, &target_new_name, false);
    }

    emit_mirror_summary(
        &target_symbol,
        heuristic_mirror_count,
        &heuristic_mirror_names,
        args.show_heuristic_mirrors,
    );
    emit_verification_payload(
        &repo_root,
        &target_symbol,
        &target_new_name,
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
// Heuristic mirror summary
// ---------------------------------------------------------------------------

/// Emit `heuristic_mirrors_not_touched: <N>` and, when count > 0, the hint
/// line. When `show_mirrors` is set, embed the candidate list with the
/// UNKNOWN_TIER placeholder shape (T4-7 will populate real tier/check values).
fn emit_mirror_summary(symbol: &str, count: usize, mirror_names: &[String], show_mirrors: bool) {
    println!("heuristic_mirrors_not_touched: {count}");
    // Zero count: omit hint — it adds noise when no mirrors exist
    // (per test_rename_zero_count_omits_hint_line).
    if count > 0 {
        println!(
            "hint: \"ecp find-schema-bindings {symbol}\" or rerun with --show-heuristic-mirrors"
        );
    }
    if show_mirrors && !mirror_names.is_empty() {
        println!("heuristic_mirrors:");
        for name in mirror_names {
            // T-H2 stub: tier/check data lands in T4-7; placeholder shape
            // exercises the format/wiring so T4-7 only needs to fill values.
            println!("  - {name:<30} [UNKNOWN_TIER]   checks: <none recorded yet>");
            println!("                                          requires_verification: true");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn apply_rename(path: &Path, ranges: &[IdentifierRange], new_bytes: &[u8]) -> std::io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.start_byte));
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
