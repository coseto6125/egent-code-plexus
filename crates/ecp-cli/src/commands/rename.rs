//! `ecp rename` — AST-powered multi-lang rename.
//!
//! Pipeline:
//! 1. **Plan (graph)**: resolve the target through the one target resolver
//!    (`read_context::resolve_target`), with the overlay, collect inbound-edge
//!    source files + the target's own file. Refuses an ambiguous target, and
//!    refuses to write from a warm-attached graph.
//! 2. **Verify (AST)**: tree-sitter parse each affected file, find every
//!    `identifier` byte-range whose text matches the target, and keep the
//!    ones the graph binds to the declaration (see [`Binding`]).
//!    Supported languages: Python, TypeScript/TSX, JavaScript,
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

use crate::read_context::{ambiguity_candidates, resolve_target, target_request};
use clap::Args;
use ecp_analyzer::identifier_finder::{find_identifier_occurrences, has_identifier_finder};
use ecp_core::analyzer::types::IdentifierRange;
use ecp_core::contracts::identity::{Candidate, Qualifier, TargetRequest, TargetResolution};
use ecp_core::file_category::determine_category;
use ecp_core::graph::{FileCategory, NodeKind};
use ecp_core::registry::atomic_write_bytes;
use ecp_core::session::MergedGraph;
use ecp_core::EcpError;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// AST-powered multi-language rename: locates the identifier occurrences the
/// graph binds to one declaration via tree-sitter and rewrites them
/// atomically, with optional dry-run preview.
/// It edits the declaration's name, the occurrences inside its referrers, and
/// the occurrences outside every function. It leaves an occurrence inside any
/// other function, and lists each one in the payload's `unbound` field.
/// When the target matches several declarations, it exits non-zero, writes
/// nothing, and lists each declaration under `candidates` with the arguments
/// that select it alone.
/// Without `--dry-run` it refuses a warm-attached graph and names the
/// reindex command.
/// The closing toon payload gains a `result` field only when the answer carries
/// a caveat (a warm-attached or behind-HEAD graph, or uncommitted paths that
/// could not be read); its absence means the graph was current. It gains a
/// `not_scanned` list (file, reason) when an affected file has no identifier
/// finder or cannot be read; those files are left unchanged. The dry-run
/// headline then reads `risk partial; files N; usages M; not scanned K`
/// instead of `risk safe; files N; usages M`.
#[derive(Args, Debug, Clone, Serialize)]
pub struct RenameArgs {
    /// The declaration to rename (equivalent to `--symbol` flag): `name`,
    /// `Owner.name`, `::name` for a declaration with no owner, or `name@LINE`.
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
    /// count is shown. Tier shows the `unresolved` sentinel until T4-7
    /// populates real tier/check values.
    #[arg(long, default_value_t = false)]
    pub show_heuristic_mirrors: bool,

    /// Select the declaration by its file path when the target matches
    /// several. A path equal to the value wins, then a path that ends with
    /// `/<value>`, then a path that contains it.
    #[arg(long = "file", alias = "file_path", alias = "file-path")]
    pub file: Option<String>,

    /// Select the declaration by its kind when the target matches several:
    /// function, method, class, struct, and the other node kinds.
    #[arg(long)]
    pub kind: Option<String>,
}

// ---------------------------------------------------------------------------
// Occurrence record for post-rename verification
// ---------------------------------------------------------------------------

/// The reason a file without an identifier finder is not scanned.
const NO_FINDER: &str = "no identifier finder for this file type";

/// An affected file that the rename did not scan, so its references stay.
#[derive(Debug, Serialize)]
struct NotScanned {
    file: String,
    reason: String,
}

impl NotScanned {
    fn new(file: &str, reason: String) -> Self {
        Self {
            file: file.to_owned(),
            reason,
        }
    }
}

/// Adds `not_scanned` only when it holds a file: its absence means every
/// affected file was scanned.
fn attach_not_scanned(payload: &mut serde_json::Value, not_scanned: &[NotScanned]) {
    if !not_scanned.is_empty() {
        payload["not_scanned"] = json!(not_scanned);
    }
}

/// One occurrence of the name, 1-based line.
#[derive(Debug, Serialize)]
struct Site {
    file: String,
    line: u32,
}

/// Adds `unbound` only when it holds a site: each occurrence of the name the
/// graph does not bind to the target, which the rename left unchanged.
fn attach_unbound(payload: &mut serde_json::Value, unbound: &[Site]) {
    if !unbound.is_empty() {
        payload["unbound"] = json!(unbound);
    }
}

fn line_of(range: &IdentifierRange) -> u32 {
    range.row as u32 + 1
}

/// Why a rename without `--dry-run` stops on a warm-attached graph.
const WARM_ATTACH_REFUSAL: &str = "rename refuses to write files: the graph is a warm-attach \
     from a sibling commit, so its occurrences can be missing or misplaced. Run \
     `ecp admin index --force --repo .`, then run the rename again. `--dry-run` answers \
     from this graph, with a caveat.";

/// The target as docs and prose spell it: the text without a root `::` or
/// `.` and without `@line`. The markdown pass and the residual scan read it.
fn doc_text<'t>(text: &'t str, request: &TargetRequest) -> &'t str {
    let text = text.trim();
    let text = match request.line() {
        Some(line) => text
            .strip_suffix(format!("@{line}").as_str())
            .unwrap_or(text),
        None => text,
    };
    match request.qualifier() {
        Qualifier::Root => text
            .strip_prefix("::")
            .or_else(|| text.strip_prefix('.'))
            .unwrap_or(text),
        _ => text,
    }
}

/// What the graph binds to the chosen declaration. An occurrence of the name
/// is bound when it lies in the declaration's own span, when its innermost
/// enclosing function refers to the declaration, or outside every function (imports,
/// module-level code, class bodies). An occurrence inside any other function
/// is another binding of the same spelling: a local, a parameter, a
/// namesake.
struct Binding<'a> {
    target_file: &'a str,
    target_lines: std::ops::RangeInclusive<u32>,
    /// The target's file and every file that holds a deterministic referrer.
    files: BTreeSet<&'a str>,
    /// Merged indices: the target and its deterministic referrers.
    referrers: FxHashSet<u32>,
    /// Function, method and constructor spans of each file in `files`:
    /// (start line, end line, merged index).
    callables: FxHashMap<&'a str, Vec<(u32, u32, u32)>>,
}

impl<'a> Binding<'a> {
    /// `on_mirror` sees the far end of each heuristic edge of the target: a
    /// heuristic edge never extends the files a rename writes.
    fn of(merged: &MergedGraph<'a>, target: &Candidate, mut on_mirror: impl FnMut(&str)) -> Self {
        let node = merged
            .node(target.node)
            .expect("the resolver answers merged-space indices");
        let target_file = node.file_path(merged).unwrap_or_default();
        let target_name = node.name(merged);
        let mut files = BTreeSet::from([target_file]);
        let mut referrers = FxHashSet::from_iter([target.node]);
        for edge in merged.in_edges(target.node) {
            let Some(source) = merged.node(edge.source) else {
                continue;
            };
            if edge.rel_type().is_heuristic() {
                on_mirror(source.name(merged));
                continue;
            }
            if let Some(file) = source.file_path(merged) {
                files.insert(file);
            }
            // The first overload references each later one; that edge binds
            // no call of the name, so the sibling's body is not a referrer.
            if source.name(merged) != target_name || edge.source == target.node {
                referrers.insert(edge.source);
            }
        }
        for edge in merged.out_edges(target.node) {
            if edge.rel_type().is_heuristic() {
                if let Some(far) = merged.node(edge.target) {
                    on_mirror(far.name(merged));
                }
            }
        }
        Self {
            target_file,
            target_lines: node.start_line()..=node.end_line(),
            callables: callable_spans(merged, &files),
            files,
            referrers,
        }
    }

    fn binds(&self, file: &str, line: u32) -> bool {
        if file == self.target_file && self.target_lines.contains(&line) {
            return true;
        }
        // The innermost enclosing function owns the occurrence: a nested
        // function's parameter shadows the name inside its referrer parent.
        let innermost = self
            .callables
            .get(file)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|&&(start, end, _)| (start..=end).contains(&line))
            .min_by_key(|&&(start, end, _)| (end - start, std::cmp::Reverse(start)));
        innermost.is_none_or(|&(_, _, idx)| self.referrers.contains(&idx))
    }
}

/// The function, method and constructor spans of `files` in merged space:
/// visible base nodes, then the overlay's nodes, which carry on-disk spans
/// for a dirty file.
fn callable_spans<'a>(
    merged: &MergedGraph<'a>,
    files: &BTreeSet<&'a str>,
) -> FxHashMap<&'a str, Vec<(u32, u32, u32)>> {
    let callable = |kind: NodeKind| {
        matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constructor
        )
    };
    let mut out: FxHashMap<&'a str, Vec<(u32, u32, u32)>> = FxHashMap::default();
    for idx in 0..merged.node_count() {
        let Some(node) = merged.node(idx) else {
            continue;
        };
        if !callable(node.kind()) || (idx < merged.base_len() && !merged.base_visible(idx)) {
            continue;
        }
        if let Some(file) = node.file_path(merged).filter(|f| files.contains(f)) {
            out.entry(file)
                .or_default()
                .push((node.start_line(), node.end_line(), idx));
        }
    }
    out
}

#[derive(Debug, Serialize)]
struct Occurrence {
    file: String,
    line: u32,
    context: String,
}

/// `rel_path` is relative to the repo root, so the classifier never reads
/// the directories the repo happens to live in (`/tmp/test/repo`).
fn classify_context(rel_path: &str) -> String {
    let s = rel_path;
    if determine_category(s) == FileCategory::Test {
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
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
        let context = classify_context(&rel);
        for (idx, line) in content.lines().enumerate() {
            if pattern.is_match(line) {
                hits.push(Occurrence {
                    file: rel.clone().into_owned(),
                    line: (idx + 1) as u32,
                    context: context.clone(),
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
    // O(log N + collisions) via the v9 name_index instead of scanning all
    // nodes. The legacy O(N) scan is preserved as a fallback when the index
    // is empty (legacy graph.bin from v8 — engine::load already rejects
    // these, but defensively).
    let mut locs = Vec::new();
    if !graph.name_index.is_empty() {
        for idx in graph.nodes_by_name(new_name) {
            let node = &graph.nodes[idx as usize];
            let file_path = if node.has_owning_file() {
                graph.files[node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_owned()
            } else {
                "<unknown>".to_owned()
            };
            let start_line = node.start_line();
            locs.push(format!("{file_path}:{start_line}"));
        }
        return locs;
    }
    for node in graph.nodes.iter() {
        if node.name.resolve(&graph.string_pool) == new_name {
            let file_path = if node.has_owning_file() {
                graph.files[node.file_idx.to_native() as usize]
                    .path
                    .resolve(&graph.string_pool)
                    .to_owned()
            } else {
                "<unknown>".to_owned()
            };
            let start_line = node.start_line();
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
    // Graph paths are relative to the worktree top, so a run from a
    // subdirectory (or `--repo <subdir>`) joins them onto the top, not the cwd.
    let repo_root =
        crate::read_context::worktree_top(args.repo.as_deref().map_or(Path::new("."), Path::new));

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

    let request = target_request(&target_symbol, args.kind.as_deref(), args.file.as_deref())?;
    // `ast_target_name` / `ast_new_name` are the bare identifiers used for
    // tree-sitter search and byte-level rewrite.
    // "Foo.validate" → ast_target_name="validate"; "Foo.check" → ast_new_name="check"
    let ast_target_name: &str = request.name();
    let ast_new_name: &str = target_new_name
        .rsplit_once('.')
        .map(|(_, name)| name)
        .unwrap_or(&target_new_name);
    let doc_target = doc_text(&target_symbol, &request);

    if !args.dry_run && engine.is_stale_for_sha {
        return Err(EcpError::InvalidArgument(WARM_ATTACH_REFUSAL.to_string()));
    }

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

    // Stage 1: resolve the target, with the overlay, so an uncommitted
    // declaration is found and a dirty file is read at its on-disk spans.
    let view = engine.overlay_dir().and_then(|_| engine.overlay_view());
    let merged = MergedGraph::new(graph, view);
    let target = match resolve_target(&merged, &request) {
        TargetResolution::Unique(c) => Some(c),
        TargetResolution::NotFound { .. } => None,
        TargetResolution::Ambiguous(a) => {
            let payload = json!({
                "operation": "refused",
                "old": target_symbol,
                "new": target_new_name,
                "candidates": ambiguity_candidates(&a),
            });
            let _ = crate::output::emit_with_caveat(
                &payload,
                crate::output::OutputFormat::Toon,
                engine.caveat(),
            );
            return Err(a.to_error(&request));
        }
    };

    // Stage 2: parse each file, find identifier occurrences, keep the bound ones.
    let mut hits: Vec<(PathBuf, Vec<IdentifierRange>)> = Vec::new();
    // Affected files the rename could not scan. Each one is reported, so a
    // reference in a `.vue` file is never mistaken for "no reference".
    let mut not_scanned: Vec<NotScanned> = Vec::new();
    // Occurrences of the name that the graph does not bind to the target.
    let mut unbound: Vec<Site> = Vec::new();

    // Heuristic mirror count + candidate names (T-H2).
    // Count is informational only — never drives mutation.
    let mut heuristic_mirror_count: usize = 0;
    let mut heuristic_mirror_names: Vec<String> = Vec::new();

    if let Some(target) = &target {
        let binding = Binding::of(&merged, target, |name| {
            heuristic_mirror_count += 1;
            if args.show_heuristic_mirrors {
                heuristic_mirror_names.push(name.to_string());
            }
        });
        for &rel_path in &binding.files {
            if !has_identifier_finder(rel_path) {
                not_scanned.push(NotScanned::new(rel_path, NO_FINDER.to_string()));
                continue;
            }
            let abs_path = repo_root.join(rel_path);
            let bytes = match std::fs::read(&abs_path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    not_scanned.push(NotScanned::new(rel_path, format!("cannot read file: {e}")));
                    continue;
                }
            };
            let Some(occurrences) = find_identifier_occurrences(rel_path, &bytes, ast_target_name)
            else {
                not_scanned.push(NotScanned::new(rel_path, NO_FINDER.to_string()));
                continue;
            };
            let (bound, left): (Vec<_>, Vec<_>) = occurrences
                .into_iter()
                .partition(|r| binding.binds(rel_path, line_of(r)));
            unbound.extend(left.iter().map(|r| Site {
                file: rel_path.to_string(),
                line: line_of(r),
            }));
            if !bound.is_empty() {
                hits.push((abs_path, bound));
            }
        }
        not_scanned.sort_by(|a, b| a.file.cmp(&b.file));
    }

    let total_ast_hits: usize = hits.iter().map(|(_, r)| r.len()).sum();

    // Zero-occurrence case: explicit message + suggestions.
    if target.is_none() || total_ast_hits == 0 {
        if not_scanned.is_empty() {
            println!("No occurrences of \"{}\" found.", target_symbol);
        } else {
            println!(
                "No occurrences of \"{}\" found in scanned files; {} affected file(s) not scanned.",
                target_symbol,
                not_scanned.len()
            );
        }

        // Check if the name exists in doc files even though not in graph.
        let doc_hits = if args.markdown {
            apply_markdown_rename(&repo_root, doc_target, &target_new_name, true)
        } else {
            // Still scan to provide hints.
            apply_markdown_rename(&repo_root, doc_target, &target_new_name, true)
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
        // A stale or warm-attached graph is the likeliest reason a symbol the
        // agent just wrote is absent: the verdict must carry that reason. The
        // payload appears only with a caveat or an unscanned file, so the
        // plain output stays as is.
        let caveat = engine.caveat();
        if caveat.is_some() || !not_scanned.is_empty() || !unbound.is_empty() {
            let mut payload = json!({
                "operation": if args.dry_run { "dry-run" } else { "applied" },
                "old": target_symbol,
                "new": target_new_name,
                "rename_count": 0,
            });
            attach_not_scanned(&mut payload, &not_scanned);
            attach_unbound(&mut payload, &unbound);
            let _ = crate::output::emit_with_caveat(
                &payload,
                crate::output::OutputFormat::Toon,
                caveat,
            );
        }
        return Ok(());
    }

    // --- Stage 3a: dry-run — print summary + diff preview, then verification. ---
    if args.dry_run {
        if not_scanned.is_empty() {
            println!("risk safe; files {}; usages {}", hits.len(), total_ast_hits);
        } else {
            println!(
                "risk partial; files {}; usages {}; not scanned {}",
                hits.len(),
                total_ast_hits,
                not_scanned.len()
            );
        }
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
            let md_changed = apply_markdown_rename(&repo_root, doc_target, &target_new_name, true);
            if !md_changed.is_empty() {
                println!("[markdown] would update {} doc file(s):", md_changed.len());
                for (p, c) in &md_changed {
                    println!("  {} ({} occurrences)", p.display(), c);
                }
            }
        }

        emit_mirror_summary(
            doc_target,
            heuristic_mirror_count,
            &heuristic_mirror_names,
            args.show_heuristic_mirrors,
        );
        emit_verification_payload(
            &repo_root,
            doc_target,
            &target_new_name,
            total_ast_hits,
            true,
            &not_scanned,
            &unbound,
            engine,
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
        apply_markdown_rename(&repo_root, doc_target, &target_new_name, false);
    }

    emit_mirror_summary(
        doc_target,
        heuristic_mirror_count,
        &heuristic_mirror_names,
        args.show_heuristic_mirrors,
    );
    emit_verification_payload(
        &repo_root,
        doc_target,
        &target_new_name,
        total_ast_hits,
        false,
        &not_scanned,
        &unbound,
        engine,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Verification payload
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn emit_verification_payload(
    repo_root: &Path,
    old: &str,
    new: &str,
    rename_count: usize,
    dry_run: bool,
    not_scanned: &[NotScanned],
    unbound: &[Site],
    engine: &crate::engine::Engine,
) {
    let residuals = scan_word_occurrences(repo_root, old);
    let new_distribution = scan_word_occurrences(repo_root, new);

    let mut payload = json!({
        "operation": if dry_run { "dry-run" } else { "applied" },
        "old": old,
        "new": new,
        "rename_count": rename_count,
        "residuals": residuals,
        "new_distribution": new_distribution,
    });
    attach_not_scanned(&mut payload, not_scanned);
    attach_unbound(&mut payload, unbound);

    // Best-effort: emit as structured JSON after main output.
    let _ = crate::output::emit_with_caveat(
        &payload,
        crate::output::OutputFormat::Toon,
        engine.caveat(),
    );
}

// ---------------------------------------------------------------------------
// Heuristic mirror summary
// ---------------------------------------------------------------------------

/// Emit `heuristic_mirrors_not_touched: <N>` and, when count > 0, the hint
/// line. When `show_mirrors` is set, embed the candidate list with the
/// `unresolved` tier sentinel (T4-7 will populate real tier/check values).
fn emit_mirror_summary(symbol: &str, count: usize, mirror_names: &[String], show_mirrors: bool) {
    println!("heuristic_mirrors_not_touched: {count}");
    // Zero count: omit hint — it adds noise when no mirrors exist
    // (per test_rename_zero_count_omits_hint_line).
    if count > 0 {
        println!(
            "hint: \"ecp heuristics schema-bindings {symbol}\" or rerun with --show-heuristic-mirrors"
        );
    }
    if show_mirrors && !mirror_names.is_empty() {
        println!("heuristic_mirrors:");
        for name in mirror_names {
            // tier/check data lands in T4-7; until then emit the `unresolved`
            // tier sentinel (mirrors the JSON `tier` field) instead of a
            // placeholder that reads like a real tier named UNKNOWN_TIER.
            println!("  - {name:<30} tier: unresolved   requires_verification: true");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_context_root_conftest_is_test() {
        // The old substring rule needed a `/tests/` segment.
        assert_eq!(classify_context("conftest.py"), "test");
        assert_eq!(classify_context("tests/conftest.py"), "test");
    }

    #[test]
    fn test_classify_context_testimonials_dir_not_test() {
        assert_eq!(classify_context("src/testimonials/x.py"), "code");
    }

    #[test]
    fn test_classify_context_flow_tests_rs_is_test() {
        assert_eq!(classify_context("crates/x/src/flow/tests.rs"), "test");
    }

    #[test]
    fn test_classify_context_other_labels_unchanged() {
        assert_eq!(classify_context("src/lib.rs"), "code");
        assert_eq!(classify_context("docs/guide.md"), "markdown");
        assert_eq!(classify_context("Cargo.toml"), "data");
        assert_eq!(classify_context("ci.yml"), "data");
    }

    #[test]
    fn test_scan_word_occurrences_repo_under_test_dir_is_not_all_test() {
        // A repo checked out under a directory named `test` must classify by
        // its own layout, not by the absolute path above the root.
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("test").join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn needle() {}\n").unwrap();
        let hits = scan_word_occurrences(&root, "needle");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].context, "code");
    }
}
