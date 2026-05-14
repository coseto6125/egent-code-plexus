//! `gnx detect_changes` — list symbols changed by git diff and assess
//! the blast radius via affected Process execution-flows.
//!
//! Algorithm (Plan B — partial re-analyze + content-hash symbol diff):
//!   1. Run `git diff -U0` for the requested scope to get changed file paths
//!   2. For each changed file, fetch the committed ("old") content via
//!      `git show HEAD:<path>` and parse it; also parse the working-tree
//!      ("new") content.
//!   3. Hash each node's body lines (start_row..=end_row) for both sides.
//!   4. Diff by (kind, name, file) key:
//!      - new-only                → change_type="added"
//!      - old-only                → change_type="removed"
//!      - both, hash differs      → change_type="modified"
//!      - both, hash same         → skip (line-shift only; body unchanged)
//!   5. For modified/removed, find the node index in the archived graph
//!      to power the existing affected-process BFS.

use crate::commands::format::kind_to_str;
use crate::engine::Engine;
use crate::git::{DiffScope, GitDiffProvider, ShellGitProvider};
use crate::output::{emit, OutputFormat};
use crate::reanalyze::make_pipeline;
use clap::Args;
use gnx_core::algorithms::process_trace::is_test_path;
use gnx_core::graph::{ArchivedNodeKind, NodeKind};
use gnx_core::HIGH_TRUST_CONFIDENCE;
use gnx_core::graph_query::processes_containing;
use gnx_core::GnxError;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct DetectChangesArgs {
    /// Diff scope: `unstaged` (default), `staged`, `all`, or `compare`.
    #[arg(long, default_value = "unstaged")]
    pub scope: String,

    /// Required when `--scope compare`: the ref to diff against (e.g. `HEAD~1`).
    #[arg(long)]
    pub base_ref: Option<String>,

    /// Path to the repo root (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: `toon` (default), `json`, or `text`.
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,

    /// Filter changes to specific NodeKinds (comma-separated, e.g.
    /// `function,method,class`). When omitted, all kinds are reported.
    #[arg(long)]
    pub kind: Option<String>,

    /// Include test-file hunks (default: false — test files dropped).
    #[arg(long, default_value_t = false)]
    pub include_tests: bool,

    /// Drop affected processes whose execution trace traverses any edge with
    /// confidence < 0.8 (e.g. framework-aware refs from FastAPI `Depends()`,
    /// Axum/Express route handlers). Default off.
    #[arg(long, default_value_t = false)]
    pub high_trust_only: bool,
}

pub fn run(args: DetectChangesArgs, engine: &Engine) -> Result<(), GnxError> {
    let repo_path = PathBuf::from(args.repo.as_deref().unwrap_or("."));
    let scope = DiffScope::parse(Some(&args.scope), args.base_ref.as_deref())?;
    let format = OutputFormat::parse(args.format.as_deref());

    let provider = ShellGitProvider;
    let file_diffs = provider.diff(&repo_path, &scope)?;

    if file_diffs.is_empty() {
        let result = json!({
            "summary": {
                "changed_count": 0,
                "affected_count": 0,
                "risk_level": "none",
                "message": "No changes detected."
            },
            "changed_symbols": [],
            "affected_processes": [],
        });
        return emit(&result, format);
    }

    let graph = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;
    let kind_filter = parse_kind_filter(args.kind.as_deref());

    // === Plan B: partial re-analyze + line-hash symbol diff ===
    let changed_paths: Vec<String> = file_diffs
        .iter()
        .filter(|fd| args.include_tests || !is_test_path(&fd.file_path))
        .map(|fd| fd.file_path.clone())
        .collect();
    let changed_files_counted = changed_paths.len();

    // key = (kind_str, file_relpath, name) → (body_hash, start_row 0-based)
    let mut new_map: HashMap<(String, String, String), (u64, u32)> = HashMap::new();
    let mut old_map: HashMap<(String, String, String), u64> = HashMap::new();

    let pipeline = make_pipeline();

    for rel_path in &changed_paths {
        // ── new side: working-tree file ───────────────────────────────────────
        let abs = repo_path.join(rel_path);
        if abs.exists() {
            if let Ok(src) = std::fs::read(&abs) {
                let rel_pb = PathBuf::from(rel_path);
                if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &src) {
                    let lines: Vec<&[u8]> = src.split(|&b| b == b'\n').collect();
                    for raw in &lg.nodes {
                        if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                            continue;
                        }
                        let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                        let kind_str = node_kind_to_str(&raw.kind).to_string();
                        let key = (kind_str, rel_path.clone(), raw.name.clone());
                        new_map.insert(key, (h, raw.span.0));
                    }
                }
            }
        }

        // ── old side: committed HEAD content ──────────────────────────────────
        if let Some(old_src) = head_blob(&repo_path, rel_path) {
            let rel_pb = PathBuf::from(rel_path);
            if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &old_src) {
                let lines: Vec<&[u8]> = old_src.split(|&b| b == b'\n').collect();
                for raw in &lg.nodes {
                    if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                        continue;
                    }
                    let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                    let kind_str = node_kind_to_str(&raw.kind).to_string();
                    let key = (kind_str, rel_path.clone(), raw.name.clone());
                    old_map.insert(key, h);
                }
            }
        }
    }

    // Build old-graph lookup: (kind_str, file_path, name) → node_idx
    let changed_files_set: HashSet<&str> = changed_paths.iter().map(|s| s.as_str()).collect();
    let mut old_graph_idx: HashMap<(String, String, String), u32> = HashMap::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if matches!(
            node.kind,
            ArchivedNodeKind::File | ArchivedNodeKind::Process
        ) {
            continue;
        }
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file_path = file_node.path.resolve(&graph.string_pool);
        if !changed_files_set.contains(file_path) {
            continue;
        }
        let kind_str = kind_to_str(&node.kind).to_string();
        let name = node.name.resolve(&graph.string_pool).to_string();
        old_graph_idx.insert((kind_str, file_path.to_string(), name), idx as u32);
    }

    let mut changed_symbols: Vec<Value> = Vec::new();
    let mut changed_node_indices: Vec<u32> = Vec::new();

    // added: in new but not in old
    for (key, (_, start_row)) in &new_map {
        if old_map.contains_key(key) {
            continue;
        }
        let kind_str = &key.0;
        if !kind_matches_str(kind_str, &kind_filter) {
            continue;
        }
        changed_symbols.push(json!({
            "name": key.2,
            "type": kind_str,
            "filePath": key.1,
            "line": start_row,
            "change_type": "added",
        }));
    }

    // modified / removed: in old_map
    for (key, old_hash) in &old_map {
        let kind_str = &key.0;
        if !kind_matches_str(kind_str, &kind_filter) {
            continue;
        }
        match new_map.get(key) {
            Some((new_hash, start_row)) => {
                if old_hash != new_hash {
                    changed_symbols.push(json!({
                        "name": key.2,
                        "type": kind_str,
                        "filePath": key.1,
                        "line": start_row,
                        "change_type": "modified",
                    }));
                    if let Some(&idx) = old_graph_idx.get(key) {
                        if !changed_node_indices.contains(&idx) {
                            changed_node_indices.push(idx);
                        }
                    }
                }
                // same hash → skip (line-shifted only, body unchanged)
            }
            None => {
                changed_symbols.push(json!({
                    "name": key.2,
                    "type": kind_str,
                    "filePath": key.1,
                    "line": 0u32,
                    "change_type": "removed",
                }));
                if let Some(&idx) = old_graph_idx.get(key) {
                    if !changed_node_indices.contains(&idx) {
                        changed_node_indices.push(idx);
                    }
                }
            }
        }
    }

    // Find affected processes
    let process_start = graph.process_start.to_native();
    let mut affected: HashMap<u32, AffectedProcess> = HashMap::new();
    for &node_idx in &changed_node_indices {
        for (proc_idx, step) in processes_containing(graph, node_idx) {
            let proc_node = &graph.nodes[proc_idx as usize];
            let entry = affected.entry(proc_idx).or_insert_with(|| {
                let k = (proc_idx - process_start) as usize;
                let off_s = graph.traces_offsets[k].to_native() as usize;
                let off_e = graph.traces_offsets[k + 1].to_native() as usize;
                let trace = &graph.traces_data[off_s..off_e];
                let step_count = trace.len() as u32;
                let mut comms: Vec<u16> = trace
                    .iter()
                    .map(|x| graph.nodes[x.to_native() as usize].community_id.to_native())
                    .filter(|&c| c != 0)
                    .collect();
                comms.sort_unstable();
                comms.dedup();
                let process_type = if comms.len() > 1 {
                    "cross_community"
                } else {
                    "intra_community"
                };
                AffectedProcess {
                    id: proc_node.uid.resolve(&graph.string_pool).to_string(),
                    name: proc_node.name.resolve(&graph.string_pool).to_string(),
                    process_type,
                    step_count,
                    changed_steps: Vec::new(),
                }
            });
            entry.changed_steps.push((
                graph.nodes[node_idx as usize]
                    .name
                    .resolve(&graph.string_pool)
                    .to_string(),
                step,
            ));
        }
    }

    // --high-trust-only: drop processes whose trace traverses any low-confidence
    // edge (framework-aware refs emit confidence < 1.0).
    if args.high_trust_only {
        affected.retain(|&proc_idx, _| {
            let k = (proc_idx - process_start) as usize;
            let off_s = graph.traces_offsets[k].to_native() as usize;
            let off_e = graph.traces_offsets[k + 1].to_native() as usize;
            let trace = &graph.traces_data[off_s..off_e];
            for pair in trace.windows(2) {
                let a = pair[0].to_native() as usize;
                let b = pair[1].to_native();
                let out_s = graph.out_offsets[a].to_native() as usize;
                let out_e = graph.out_offsets[a + 1].to_native() as usize;
                for edge in &graph.edges[out_s..out_e] {
                    if edge.target.to_native() == b
                        && edge.confidence.to_native() < HIGH_TRUST_CONFIDENCE
                    {
                        return false;
                    }
                }
            }
            true
        });
    }

    let process_count = affected.len();
    let risk_level = match process_count {
        0 => "low",
        1..=5 => "medium",
        6..=15 => "high",
        _ => "critical",
    };

    let affected_arr: Vec<_> = affected
        .into_values()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "process_type": p.process_type,
                "step_count": p.step_count,
                "changed_steps": p.changed_steps
                    .iter()
                    .map(|(s, step)| json!({ "symbol": s, "step": step }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    let result = json!({
        "summary": {
            "changed_count": changed_symbols.len(),
            "affected_count": process_count,
            "changed_files": changed_files_counted,
            "risk_level": risk_level,
        },
        "changed_symbols": changed_symbols,
        "affected_processes": affected_arr,
    });

    if format == OutputFormat::Toon {
        let compact = compact_output(&result);
        println!("{}", compact);
        return Ok(());
    }

    emit(&result, format)
}

struct AffectedProcess {
    id: String,
    name: String,
    process_type: &'static str,
    step_count: u32,
    changed_steps: Vec<(String, u32)>,
}

fn parse_kind_filter(s: Option<&str>) -> Option<Vec<String>> {
    s.map(|raw| {
        raw.split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect()
    })
}

fn kind_matches_str(kind_str: &str, filter: &Option<Vec<String>>) -> bool {
    let Some(f) = filter else {
        return true;
    };
    let lower = kind_str.to_ascii_lowercase();
    f.iter().any(|k| k == &lower)
}

/// Map `NodeKind` (live, not archived) to the same strings as `kind_to_str`.
fn node_kind_to_str(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "File",
        NodeKind::Function => "Function",
        NodeKind::Class => "Class",
        NodeKind::Method => "Method",
        NodeKind::Interface => "Interface",
        NodeKind::Constructor => "Constructor",
        NodeKind::Property => "Property",
        NodeKind::Variable => "Variable",
        NodeKind::Const => "Const",
        NodeKind::Import => "Import",
        NodeKind::Route => "Route",
        NodeKind::Process => "Process",
        NodeKind::Document => "Document",
        NodeKind::Section => "Section",
    }
}

/// Abbreviation for a NodeKind string, used in compact output.
fn kind_abbr(kind_str: &str) -> &'static str {
    match kind_str {
        "Function" | "Method" | "Constructor" => "fn",
        "Class" => "class",
        "Interface" => "interface",
        "Property" => "prop",
        "Variable" => "var",
        "Const" => "const",
        _ => "sym",
    }
}

/// Lowercase the first character of a PascalCase identifier.
/// "HandleLogin" → "handleLogin", "DbQuery" → "dbQuery".
fn pascal_to_camel(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().to_string() + c.as_str(),
    }
}

/// Build the compact toon summary for `detect-changes` output.
///
/// Format:
/// ```text
/// risk {level}  files {n}; changed {m}; flows {f}
///   ~ fn {name}:{line}  {filepath}
///   + fn {name}:{line}  {filepath}
/// flows:
///   cross {kind} {entry} -> [{kind} {terminal}, ...]
///   intra {kind} {entry} -> [{kind} {terminal}, ...]
/// ```
///
/// Changed symbols are sorted so modified (`~`) appear before added (`+`).
/// Flows are grouped by entry symbol; each entry gets one `cross` row and one
/// `intra` row. If no processes of a type exist, the list is empty (`[]`).
fn compact_output(result: &Value) -> String {
    let summary = &result["summary"];
    let risk_level = summary["risk_level"].as_str().unwrap_or("none");
    let changed_files = summary["changed_files"].as_u64().unwrap_or(0);
    let changed_count = summary["changed_count"].as_u64().unwrap_or(0);
    let affected_count = summary["affected_count"].as_u64().unwrap_or(0);

    let mut out = String::new();
    out.push_str(&format!(
        "risk {}  files {}; changed {}; flows {}",
        risk_level, changed_files, changed_count, affected_count
    ));

    // Changed symbols — sorted: modified first, then added, then removed.
    let empty_arr = vec![];
    let symbols = result["changed_symbols"].as_array().unwrap_or(&empty_arr);
    let mut sorted_syms: Vec<&Value> = symbols.iter().collect();
    sorted_syms.sort_by(|a, b| {
        let order = |ct: &str| match ct {
            "modified" => 0,
            "added" => 1,
            "removed" => 2,
            _ => 3,
        };
        let ta = a["change_type"].as_str().unwrap_or("");
        let tb = b["change_type"].as_str().unwrap_or("");
        order(ta).cmp(&order(tb)).then_with(|| {
            let na = a["name"].as_str().unwrap_or("");
            let nb = b["name"].as_str().unwrap_or("");
            na.cmp(nb)
        })
    });

    for sym in &sorted_syms {
        let prefix = match sym["change_type"].as_str().unwrap_or("") {
            "added" => "+",
            "removed" => "-",
            _ => "~",
        };
        let abbr = kind_abbr(sym["type"].as_str().unwrap_or(""));
        let name = sym["name"].as_str().unwrap_or("");
        // line field is 0-based row; display as 1-based.
        let line = sym["line"].as_u64().unwrap_or(0) + 1;
        let file = sym["filePath"].as_str().unwrap_or("");
        out.push('\n');
        out.push_str(&format!(
            "  {} {} {}:{}  {}",
            prefix, abbr, name, line, file
        ));
    }

    // Flows — group by entry, then by process_type.
    let procs = result["affected_processes"]
        .as_array()
        .unwrap_or(&empty_arr);

    // (entry_name, entry_kind_abbr) → (cross_terminals, intra_terminals)
    let mut cross_by_entry: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut intra_by_entry: HashMap<(String, String, String), Vec<String>> = HashMap::new();

    // Collect entry kinds from changed_symbols for look-up.
    let entry_kind_map: HashMap<&str, &str> = sorted_syms
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap_or(""),
                s["type"].as_str().unwrap_or(""),
            )
        })
        .collect();

    for proc in procs {
        let proc_name = proc["name"].as_str().unwrap_or("");
        let process_type = proc["process_type"].as_str().unwrap_or("");

        // Parse "HandleLogin → DbQuery" → entry="handleLogin", terminal="dbQuery"
        let arrow = " \u{2192} "; // " → "
        let (entry_raw, terminal_raw) = if let Some(pos) = proc_name.find(arrow) {
            (&proc_name[..pos], &proc_name[pos + arrow.len()..])
        } else {
            (proc_name, "")
        };

        let entry = pascal_to_camel(entry_raw);
        let terminal = pascal_to_camel(terminal_raw);
        let entry_kind = entry_kind_map
            .get(entry.as_str())
            .copied()
            .unwrap_or("Function");
        let entry_abbr = kind_abbr(entry_kind).to_string();
        // Assume terminals are Functions unless we know otherwise.
        let terminal_str = format!("fn {}", terminal);

        let key = (entry.clone(), entry_abbr.clone());
        if process_type == "intra_community" {
            intra_by_entry
                .entry((entry, entry_abbr, String::new()))
                .or_default()
                .push(terminal_str);
        } else {
            cross_by_entry.entry(key).or_default().push(terminal_str);
        }
    }

    // Collect all unique entries (preserving both cross and intra keys).
    let mut entry_keys: Vec<(String, String)> = cross_by_entry.keys().cloned().collect();
    for (e, abbr, _) in intra_by_entry.keys() {
        let k = (e.clone(), abbr.clone());
        if !entry_keys.contains(&k) {
            entry_keys.push(k);
        }
    }
    entry_keys.sort();

    if !entry_keys.is_empty() {
        out.push_str("\nflows:");
        for (entry, abbr) in &entry_keys {
            let cross_terms = cross_by_entry
                .get(&(entry.clone(), abbr.clone()))
                .cloned()
                .unwrap_or_default();
            out.push('\n');
            out.push_str(&format!(
                "  cross {} {} -> [{}]",
                abbr,
                entry,
                cross_terms.join(", ")
            ));

            let intra_terms = intra_by_entry
                .get(&(entry.clone(), abbr.clone(), String::new()))
                .cloned()
                .unwrap_or_default();
            out.push('\n');
            out.push_str(&format!(
                "  intra {} {} -> [{}]",
                abbr,
                entry,
                intra_terms.join(", ")
            ));
        }
    }

    out
}

/// FNV-64 hash of the source lines spanning [start_row, end_row] (inclusive,
/// 0-based). Normalises trailing whitespace on each line so that edits that
/// only adjust indentation are treated as content-identical. Returns 0 if the
/// row range is out of bounds (graceful degradation).
fn hash_node_lines(lines: &[&[u8]], start_row: u32, end_row: u32) -> u64 {
    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;

    let start = start_row as usize;
    let end = (end_row as usize).min(lines.len().saturating_sub(1));
    if start > end || start >= lines.len() {
        return 0;
    }

    let mut hash = FNV_OFFSET;
    for &line in &lines[start..=end] {
        let trimmed = line
            .iter()
            .rposition(|&b| b != b' ' && b != b'\t' && b != b'\r')
            .map(|pos| &line[..=pos])
            .unwrap_or(b"");
        for &byte in trimmed {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // Separator between lines so "a\nb" ≠ "ab"
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Fetch the committed (HEAD) content of a repo-relative path via
/// `git show HEAD:<path>`. Returns `None` for new files (not yet in HEAD).
fn head_blob(repo: &std::path::Path, rel_path: &str) -> Option<Vec<u8>> {
    use crate::git::safe_exec;
    let out = safe_exec::git()
        .args(["show", &format!("HEAD:{rel_path}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}
