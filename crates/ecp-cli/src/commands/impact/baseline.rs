use super::bfs::{merged_node_meta, run_bfs};
use super::coverage::{build_coverage_json, coverage_analyses};
use super::payload::{BaselinePayload, ChangedSymbol, ImpactBySymbol};
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
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Bundles the typed envelope with the enrichment inputs that only the
/// JSON-emitting path (`impact_with_baseline`) needs. Computed once in
/// [`compute_baseline`] so [`impact_with_baseline`] and
/// [`build_baseline_payload`] never re-run the diff/parse/BFS pass.
struct BaselineComputation {
    payload: BaselinePayload,
    hidden_edges_total: u64,
    hidden_heuristic_total: u64,
    per_symbol_bfs: Vec<(usize, Vec<Value>)>,
    min_conf: f32,
    rel_filter: Option<Vec<String>>,
    effective_include_tests: bool,
    /// True on the "0 changes detected" short-circuit — the pre-refactor
    /// code returned straight from that branch, skipping hidden-edge /
    /// heuristic / coverage enrichment entirely. Callers must replicate
    /// that early return rather than run enrichment over the empty payload.
    no_changes: bool,
}

/// In-process typed accessor for the baseline envelope, used by
/// `review::aggregate` to avoid `Value` string-key navigation. Enrichment
/// fields (`hidden_edges`, `heuristic_callers`, `coverage`) live only in the
/// `Value` returned by [`impact_with_baseline`] — they aren't part of the
/// typed envelope.
pub fn build_baseline_payload(
    args: &ImpactArgs,
    engine: &Engine,
) -> Result<BaselinePayload, EcpError> {
    compute_baseline(args, engine).map(|c| c.payload)
}

pub(super) fn impact_with_baseline(args: &ImpactArgs, engine: &Engine) -> Result<Value, EcpError> {
    let comp = compute_baseline(args, engine)?;
    let mut result =
        serde_json::to_value(&comp.payload).map_err(|e| EcpError::Serialization(e.to_string()))?;
    if comp.no_changes {
        return Ok(result);
    }

    attach_hidden_edges(&mut result, comp.hidden_edges_total);
    attach_heuristic_fields(
        &mut result,
        comp.hidden_heuristic_total,
        vec![],
        !args.no_heuristic,
        args.explain_confidence,
        args.confidence_threshold,
    );

    if args.test_coverage {
        let graph = engine.graph().map_err(|e| EcpError::Rkyv(e.to_string()))?;
        let view = engine.overlay_view();
        let analyses = coverage_analyses(
            graph,
            view,
            &comp.per_symbol_bfs,
            &args.direction,
            args.depth,
            comp.min_conf,
            comp.effective_include_tests,
            &comp.rel_filter,
        );
        result["coverage"] = build_coverage_json(analyses);
    }

    Ok(result)
}

fn compute_baseline(args: &ImpactArgs, engine: &Engine) -> Result<BaselineComputation, EcpError> {
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
        return Ok(BaselineComputation {
            payload: BaselinePayload {
                status: "success".to_string(),
                baseline: baseline_ref.to_string(),
                message: Some("0 changes detected — no symbols to assess".to_string()),
                changed_paths,
                changed_symbols: vec![],
                impact_by_symbol: vec![],
            },
            hidden_edges_total: 0,
            hidden_heuristic_total: 0,
            per_symbol_bfs: vec![],
            min_conf: 0.0,
            rel_filter: None,
            effective_include_tests: false,
            no_changes: true,
        });
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
    let mut changed_symbols: Vec<ChangedSymbol> = Vec::new();
    let mut changed_node_indices: Vec<usize> = Vec::new();

    for (key, (_, start_row)) in &new_map {
        if !old_map.contains_key(key) {
            changed_symbols.push(ChangedSymbol {
                name: key.2.clone(),
                kind: key.0.to_string(),
                file_path: key.1.clone(),
                line: *start_row,
                change_type: "added".to_string(),
            });
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
                    changed_symbols.push(ChangedSymbol {
                        name: key.2.clone(),
                        kind: key.0.to_string(),
                        file_path: key.1.clone(),
                        line: *start_row,
                        change_type: "modified".to_string(),
                    });
                    if let Some(&idx) = old_graph_idx.get(key) {
                        if !changed_node_indices.contains(&idx) {
                            changed_node_indices.push(idx);
                        }
                    }
                }
            }
            None => {
                changed_symbols.push(ChangedSymbol {
                    name: key.2.clone(),
                    kind: key.0.to_string(),
                    file_path: key.1.clone(),
                    line: 0,
                    change_type: "removed".to_string(),
                });
                if let Some(&idx) = old_graph_idx.get(key) {
                    if !changed_node_indices.contains(&idx) {
                        changed_node_indices.push(idx);
                    }
                }
            }
        }
    }

    // `new_map` / `old_map` are hash maps, so the two loops above visit
    // symbols in an order that varies between runs of the same binary
    // (verified: two runs, same tree, same binary, different output order).
    // A consumer asking the same question twice must get the same answer, so
    // fix the order here; `changed_node_indices` drives `impact_by_symbol`,
    // so it needs the same treatment.
    // `kind` is part of the key: a struct and its impl block can share a
    // file, line and name, and without it those two stay hash-ordered.
    changed_symbols.sort_by(|a, b| {
        (&a.file_path, a.line, &a.name, &a.kind).cmp(&(&b.file_path, b.line, &b.name, &b.kind))
    });
    changed_node_indices.sort_unstable();

    let min_conf = resolve_min_conf(args);
    let rel_filter = parse_csv_lower(args.relation_types.as_deref());
    // --test-coverage implies --include-tests so test callers are reachable.
    let effective_include_tests = args.include_tests || args.test_coverage;

    // Run BFS from each changed symbol.
    let mut impact_by_symbol: Vec<ImpactBySymbol> = Vec::new();
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
            args.max_results,
        );
        let mut sym_entry = ImpactBySymbol {
            symbol: sym_name,
            file_path: sym_file,
            impact: det_results.clone(),
            heuristic_callers: None,
            downstream_callees: None,
        };
        if !args.no_heuristic {
            sym_entry.heuristic_callers = Some(tag_heuristic(heur_results));
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
                args.max_results,
            );
            if downstream_results.len() > 1 {
                sym_entry.downstream_callees = Some(downstream_results);
            }
        }
        impact_by_symbol.push(sym_entry);
        hidden_edges_total += hidden_conf;
        hidden_heuristic_total += hidden_heur;
        per_symbol_bfs.push((start_idx, det_results));
    }

    Ok(BaselineComputation {
        payload: BaselinePayload {
            status: "success".to_string(),
            baseline: baseline_ref.to_string(),
            message: None,
            changed_paths,
            changed_symbols,
            impact_by_symbol,
        },
        hidden_edges_total,
        hidden_heuristic_total,
        per_symbol_bfs,
        min_conf,
        rel_filter,
        effective_include_tests,
        no_changes: false,
    })
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
///
/// `git_ref` comes from `--baseline`, so it is checked here rather than relying
/// on the caller having run the diff first. That ordering does hold today, but
/// it is an accident of one function's body: nothing in this signature says the
/// ref arrives validated, so a reordering or a second caller would reopen the
/// option injection without touching this file.
fn head_blob_at(repo: &std::path::Path, rel_path: &str, git_ref: &str) -> Option<Vec<u8>> {
    use crate::git::safe_exec;
    safe_exec::reject_option_like_rev(git_ref).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repo: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git available")
            .success());
    }

    fn one_commit_repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "t@example.invalid"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "one"]);
    }

    /// `head_blob_at` builds `git show <ref>:<path>` from `--baseline`. Its
    /// caller validates the ref earlier, but that is one function's call
    /// order, not this function's contract, so the guard is pinned here where
    /// a reordering or a second caller cannot quietly remove it.
    ///
    /// The fixture is a real repository and `rel_path` carries no slash, both
    /// on purpose. Without the guard, git runs
    /// `git show --output=<out>/o:lib.rs`, succeeds, and writes that file, so
    /// the empty directory is what fails when the guard goes away. The first
    /// version of this test used a non-repository and a path with a slash;
    /// git then failed for its own reasons and the test passed with the guard
    /// reverted, proving nothing.
    #[test]
    fn head_blob_at_refuses_an_option_shaped_ref_without_running_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let out_dir = tmp.path().join("out");
        one_commit_repo(&repo);
        std::fs::create_dir_all(&out_dir).unwrap();

        let injected = format!("--output={}/o", out_dir.display());
        let got = head_blob_at(&repo, "lib.rs", &injected);

        assert!(got.is_none(), "an option-shaped ref must not reach git");
        let written: Vec<String> = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            written.is_empty(),
            "git ran with the injected --output and wrote {written:?}"
        );
    }

    /// The companion. A guard that rejected every ref would pass the test
    /// above on its own; this one fails if the guard grows too wide.
    #[test]
    fn head_blob_at_still_reads_a_blob_at_an_ordinary_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        one_commit_repo(&repo);

        let blob = head_blob_at(&repo, "lib.rs", "HEAD").expect("HEAD:lib.rs must resolve");
        assert_eq!(String::from_utf8(blob).unwrap(), "fn a() {}\n");
    }
}
