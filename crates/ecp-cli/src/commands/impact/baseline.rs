use super::bfs::{merged_node_meta, run_bfs};
use super::coverage::{build_coverage_json, coverage_analyses};
use super::{parse_csv_lower, resolve_min_conf, tag_heuristic, ImpactArgs};
use crate::commands::format::{kind_to_str, node_kind_to_str};
use crate::commands::impact::{attach_heuristic_fields, attach_hidden_edges, Direction};
use crate::engine::Engine;
use crate::git::{DiffScope, GitDiffProvider, ShellGitProvider};
use crate::reanalyze::make_pipeline_for_names;
use ecp_core::algorithms::process_trace::is_test_path;
use ecp_core::graph::NodeKind;
use ecp_core::EcpError;
use rayon::prelude::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub(super) fn impact_with_baseline(args: &ImpactArgs, engine: &Engine) -> Result<Value, EcpError> {
    let baseline_ref = args.baseline.as_deref().unwrap();
    let repo_path = PathBuf::from(args.repo.as_deref().unwrap_or("."));

    let scope = DiffScope::Compare(baseline_ref.to_string());
    let provider = ShellGitProvider;
    let file_diffs = provider.diff(&repo_path, &scope)?;

    // Un-filtered file list from `git diff`. Emitted in the JSON envelope as
    // `changed_paths` so downstream consumers (pr-analyze, future area
    // classifiers) can branch on docs-only / whitespace-only / comment-only
    // diffs (which yield zero `changed_symbols`) without a second
    // `git diff --name-only` subprocess.
    let changed_paths: Vec<String> = file_diffs.iter().map(|fd| fd.file_path.clone()).collect();

    if file_diffs.is_empty() {
        return Ok(json!({
            "status": "success",
            "baseline": baseline_ref,
            "message": "0 changes detected — no symbols to assess",
            "changed_paths": changed_paths,
            "changed_symbols": [],
            "impact_by_symbol": [],
        }));
    }

    let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
    let view = engine.overlay_view();

    // Test-filtered subset for the semantic re-parse + BFS lookup. The JSON
    // envelope still emits the full `changed_paths` (above).
    let parsed_paths: Vec<String> = file_diffs
        .iter()
        .filter(|fd| args.include_tests || !is_test_path(&fd.file_path))
        .map(|fd| fd.file_path.clone())
        .collect();

    // Re-parse new and old side per changed file. Each iteration is
    // independent (writes only into its own local vectors), and tree-sitter
    // parse + `git show` subprocess dominate the work — fan out via rayon
    // and merge at the end. `pipeline.parse_file_raw` is the same call path
    // that `pipeline.analyze`'s `into_par_iter` already uses, so providers
    // are Send + Sync by construction.
    //
    // Scoped to the languages this diff actually touches (mirrors the
    // incremental-reanalyze path) instead of `make_pipeline()`'s full
    // 20-provider tree-sitter `Query` compile (~0.65s) — a 2-file diff was
    // paying that fixed cost for ~8ms of real parse work. `provider_name_for_path`
    // never routes a path to "Markdown"/"YAML" (no extension dispatch exists
    // for either — confirmed dead in `make_pipeline()` too), so skipping them
    // here changes nothing observable.
    let needed: HashSet<&str> = parsed_paths
        .iter()
        .filter_map(|p| {
            ecp_core::analyzer::pipeline::AnalyzerPipeline::provider_name_for_path(
                std::path::Path::new(p),
            )
        })
        .collect();
    let pipeline = make_pipeline_for_names(needed.iter().copied());
    type NewEntry = ((&'static str, String, String), (u64, u32));
    type OldEntry = ((&'static str, String, String), u64);

    let per_file: Vec<(Vec<NewEntry>, Vec<OldEntry>)> = parsed_paths
        .par_iter()
        .map(|rel_path| {
            let mut new_local: Vec<NewEntry> = Vec::new();
            let mut old_local: Vec<OldEntry> = Vec::new();

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
                            let kind_str = node_kind_to_str(&raw.kind);
                            new_local.push((
                                (kind_str, rel_path.clone(), raw.name.clone()),
                                (h, raw.span.0),
                            ));
                        }
                    }
                }
            }

            if let Some(old_src) = head_blob_at(&repo_path, rel_path, baseline_ref) {
                let rel_pb = PathBuf::from(rel_path);
                if let Ok(lg) = pipeline.parse_file_raw(&rel_pb, &old_src) {
                    let lines: Vec<&[u8]> = old_src.split(|&b| b == b'\n').collect();
                    for raw in &lg.nodes {
                        if matches!(raw.kind, NodeKind::File | NodeKind::Process) {
                            continue;
                        }
                        let h = hash_node_lines(&lines, raw.span.0, raw.span.2);
                        let kind_str = node_kind_to_str(&raw.kind);
                        old_local.push(((kind_str, rel_path.clone(), raw.name.clone()), h));
                    }
                }
            }

            (new_local, old_local)
        })
        .collect();

    let total_new = per_file.iter().map(|(n, _)| n.len()).sum();
    let total_old = per_file.iter().map(|(_, o)| o.len()).sum();
    let mut new_map: HashMap<(&'static str, String, String), (u64, u32)> =
        HashMap::with_capacity(total_new);
    let mut old_map: HashMap<(&'static str, String, String), u64> =
        HashMap::with_capacity(total_old);
    for (new_local, old_local) in per_file {
        new_map.extend(new_local);
        old_map.extend(old_local);
    }

    // Build lookup from old graph: (kind_str, file_path, name) → node_idx.
    let parsed_paths_set: HashSet<&str> = parsed_paths.iter().map(|s| s.as_str()).collect();
    let mut old_graph_idx: HashMap<(&'static str, String, String), usize> = HashMap::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        // Synthetic nodes (decorates_edges resolver-miss `Annotation`) carry
        // `file_idx == SYNTHETIC_FILE_IDX` (u32::MAX). Skip — they don't
        // belong to any file in `parsed_paths_set` by construction.
        if !node.has_owning_file() {
            continue;
        }
        let file_node = &graph.files[node.file_idx.to_native() as usize];
        let file_path = file_node.path.resolve(&graph.string_pool);
        if !parsed_paths_set.contains(file_path) {
            continue;
        }
        let kind_str = kind_to_str(&node.kind);
        let name = node.name.resolve(&graph.string_pool).to_string();
        old_graph_idx.insert((kind_str, file_path.to_string(), name), idx);
    }

    // Collect changed symbol keys + their graph indices.
    let mut changed_symbols: Vec<Value> = Vec::new();
    let mut changed_node_indices: Vec<usize> = Vec::new();

    for (key, (_, start_row)) in &new_map {
        if !old_map.contains_key(key) {
            changed_symbols.push(json!({
                "name": key.2,
                "kind": key.0,
                "filePath": key.1,
                "line": start_row,
                "change_type": "added",
            }));
            if let Some(&idx) = old_graph_idx.get(key) {
                if !changed_node_indices.contains(&idx) {
                    changed_node_indices.push(idx);
                }
            }
        }
    }

    for (key, old_hash) in &old_map {
        match new_map.get(key) {
            Some((new_hash, start_row)) => {
                if old_hash != new_hash {
                    changed_symbols.push(json!({
                        "name": key.2,
                        "kind": key.0,
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
            }
            None => {
                changed_symbols.push(json!({
                    "name": key.2,
                    "kind": key.0,
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

    let min_conf = resolve_min_conf(args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    // --test-coverage implies --include-tests so test callers are reachable.
    let effective_include_tests = args.include_tests || args.test_coverage;

    // Run BFS from each changed symbol.
    let mut impact_by_symbol: Vec<Value> = Vec::new();
    let mut hidden_edges_total: u64 = 0;
    let mut hidden_heuristic_total: u64 = 0;
    let mut per_symbol_bfs: Vec<(usize, Vec<Value>)> = Vec::new();
    for &base_idx in &changed_node_indices {
        let node = &graph.nodes[base_idx];
        if !node.has_owning_file() {
            continue;
        }
        // Merged-space entry: a changed symbol further edited in the working
        // tree starts at its virtual twin; one deleted on disk is skipped
        // (no node to traverse from).
        let start_idx = match view {
            Some(v) => match v.redirect(base_idx as u32) {
                Some(t) => t as usize,
                None => continue,
            },
            None => base_idx,
        };
        let meta = merged_node_meta(graph, view, start_idx);
        let (sym_name, sym_file) = (meta.name, meta.file_path);
        let (det_results, heur_results, hidden_conf, hidden_heur) = run_bfs(
            graph,
            view,
            start_idx,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
            !args.no_heuristic,
        );
        let mut sym_entry = json!({
            "symbol": sym_name,
            "filePath": sym_file,
            "impact": det_results.clone(),
        });
        if !args.no_heuristic {
            sym_entry["heuristic_callers"] = json!(tag_heuristic(heur_results));
        }
        // Orphan-symbol fallback: when upstream-only mode finds no callers,
        // attach depth-1 downstream callees so the changed symbol still
        // exposes structural signal (its callees) instead of an empty
        // `impact: []`. `det_results.len() <= 1` relies on the documented
        // `run_bfs` start-node-at-depth-0 invariant.
        if args.direction == Direction::Up && det_results.len() <= 1 {
            let (downstream_results, _, _, _) = run_bfs(
                graph,
                view,
                start_idx,
                &Direction::Down,
                1, // depth = 1, direct callees only
                min_conf,
                effective_include_tests,
                &rel_filter,
                !args.no_heuristic,
            );
            if downstream_results.len() > 1 {
                sym_entry["downstream_callees"] = json!(downstream_results);
            }
        }
        impact_by_symbol.push(sym_entry);
        hidden_edges_total += hidden_conf;
        hidden_heuristic_total += hidden_heur;
        per_symbol_bfs.push((start_idx, det_results));
    }

    let mut result = json!({
        "status": "success",
        "baseline": baseline_ref,
        "changed_paths": changed_paths,
        "changed_symbols": changed_symbols,
        "impact_by_symbol": impact_by_symbol,
    });
    attach_hidden_edges(&mut result, hidden_edges_total);
    attach_heuristic_fields(
        &mut result,
        hidden_heuristic_total,
        vec![],
        !args.no_heuristic,
        args.explain_confidence,
        args.confidence_threshold,
    );

    if args.test_coverage {
        let analyses = coverage_analyses(
            graph,
            view,
            &per_symbol_bfs,
            &args.direction,
            args.depth,
            min_conf,
            effective_include_tests,
            &rel_filter,
        );
        result["coverage"] = build_coverage_json(analyses);
    }

    Ok(result)
}

/// FNV-64 hash of the source lines spanning [start_row, end_row] (inclusive,
/// 0-based). Normalises trailing whitespace so indent-only edits are stable.
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
        hash ^= b'\n' as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Fetch the content of a repo-relative path at a specific git ref via
/// `git show <ref>:<path>`. Returns `None` for paths not present at that ref.
fn head_blob_at(repo: &std::path::Path, rel_path: &str, git_ref: &str) -> Option<Vec<u8>> {
    use crate::git::safe_exec;
    let out = safe_exec::git()
        .args(["show", &format!("{git_ref}:{rel_path}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}
