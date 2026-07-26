use clap::Args;
use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::pipeline::AnalyzerPipeline;
use ignore::WalkBuilder;
use rustc_hash::FxHashSet;

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    #[arg(long)]
    pub repo: String,

    /// Force-rebuild L2 at the target SHA. Drops the existing L2 dir and any
    /// orphan `.building/`, invalidates L1 sessions that have overlays for
    /// this SHA (clean sessions kept), drops the per-file `parse_cache/` so
    /// cached parser outputs from earlier binaries don't replay, then
    /// rebuilds. Without `--force`, an existing L2 is reused. Use after
    /// analyzer/grammar upgrade or to recover from L2 corruption.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Optional path to write a JSONL dump of every resolver decision.
    /// Used by the oracle verification harness; off by default.
    /// Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
    #[arg(long)]
    pub dump_resolver: Option<std::path::PathBuf>,

    /// Suppress progress output (timings, "Graph saved", etc.). Used by
    /// auto_ensure when an agent command transparently rebuilds; the
    /// agent's stdout must stay clean and the user sees only the single
    /// "Index refreshed" notice from the wrapper.
    #[arg(skip)]
    pub quiet: bool,
}

/// Analyzer pipeline: walk `src_root`, parse all recognized source files,
/// build a `ZeroCopyGraph`, write `graph.bin` and a tantivy full-text index
/// into `out_dir`.
///
/// The caller is responsible for:
/// - creating `out_dir` before calling (use `std::fs::create_dir_all`).
/// - all registry / branch-meta bookkeeping (this function is pure I/O).
///
/// `parse_cache_root`, when `Some`, enables the persistent per-file parse
/// cache rooted at `<repo_root>/parse_cache/<fp>/`. Cache reads are
/// best-effort: misses / corruption fall back to a fresh parse. Bypassed
/// when env `ECP_NO_CACHE=1` is set — matches `--no-cache` flag semantics.
///
/// Returns `(node_count, global_graph)`. The orchestrator owns the graph
/// after this call so it can dispatch the tantivy build to a background
/// thread AFTER renaming `building` → `publish_dir`, which avoids the
/// stale-path race where tantivy writes to a directory that's already been
/// renamed out from under it.
pub fn run_analyzer_for_paths(
    src_root: &std::path::Path,
    out_dir: &std::path::Path,
    parse_cache_root: Option<&std::path::Path>,
    dump_resolver: Option<&std::path::Path>,
) -> std::io::Result<(usize, ecp_core::graph::ZeroCopyGraph)> {
    let prof = std::env::var("ECP_PROF").is_ok();
    let t_step1 = std::time::Instant::now();
    // ── Step 1: Scan files (parallel walker) ──────────────────────────────
    // `WalkBuilder::build_parallel()` fans the directory traversal across
    // rayon-style worker threads. Each visitor pushes accepted paths into
    // an MPSC channel; the main thread drains into the analysis list.
    // Sequential `.build()` was ~100ms on .sample_repo's 22.7k entries —
    // parallel cuts the syscall-bound stat/readdir tax.
    // Per-file byte cap: `ECP_MAX_FILE_BYTES` env > `[index] max_file_bytes`
    // in `.ecp/config.toml` > 1 MiB default. See `ecp_core::config`.
    let max_file_size = ecp_core::config::resolve_max_file_bytes(src_root);
    let (file_tx, file_rx) = std::sync::mpsc::channel::<(std::path::PathBuf, std::path::PathBuf)>();
    let skipped_large = std::sync::atomic::AtomicU64::new(0);
    let skipped_large_ref = &skipped_large;
    let max_file_size_ref = &max_file_size;
    let src_root_ref = src_root;
    let mut walk_builder = WalkBuilder::new(src_root);
    walk_builder
        .hidden(false)
        .add_custom_ignore_filename(crate::ECP_IGNORE_FILE);
    // Prune LLM-agent worktree containers + nested git worktrees so the
    // graph doesn't double-count symbols from sibling source copies.
    // Override via `ECP_INCLUDE_WORKTREES=1` (see `walker_filter` docs).
    let filter_root = src_root_ref.to_path_buf();
    walk_builder.filter_entry(move |entry| {
        !crate::walker_filter::is_skippable_worktree_descendant(entry.path(), &filter_root)
    });
    walk_builder.build_parallel().run(|| {
        let tx = file_tx.clone();
        Box::new(move |result| {
            if let Ok(entry) = result {
                let path = entry.path();
                if path.is_file() && should_analyze_path(path) {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.len() > *max_file_size_ref {
                            skipped_large_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            return ignore::WalkState::Continue;
                        }
                    }
                    let rel_path = path.strip_prefix(src_root_ref).unwrap_or(path);
                    let _ = tx.send((path.to_path_buf(), rel_path.to_path_buf()));
                }
            }
            ignore::WalkState::Continue
        })
    });
    drop(file_tx);
    let files_to_analyze: Vec<(std::path::PathBuf, std::path::PathBuf)> =
        file_rx.into_iter().collect();
    let skipped_large_files = skipped_large.load(std::sync::atomic::Ordering::Relaxed);

    if skipped_large_files > 0 {
        tracing::warn!(
            "Skipped {} files > {} KB during analysis of {:?}",
            skipped_large_files,
            max_file_size / 1024,
            src_root
        );
    }

    if prof {
        eprintln!(
            "prof step1.scan_files: {:.2}s ({} files)",
            t_step1.elapsed().as_secs_f32(),
            files_to_analyze.len()
        );
    }
    let t_step2 = std::time::Instant::now();
    // ── Step 2: Initialize pipeline with only needed providers ────────────
    //
    // Each provider's `::new()` builds a tree-sitter Query from a static
    // `.scm` source (s-expression parse + bytecode generation). On a
    // 14-lang corpus, ~24 providers × 5-15ms = ~280ms serial. Run them
    // concurrently via rayon — Query::new is CPU-bound + thread-safe;
    // construction order doesn't matter for runtime semantics (file
    // routing is keyed on the lowercase `name()` string, not ordering).
    let needed = detect_needed_providers(&files_to_analyze);
    let needed_names: Vec<&'static str> = crate::provider_registry::PROVIDER_CONSTRUCTORS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| needed.contains(*name))
        .collect();

    use rayon::prelude::*;
    let providers: Vec<Box<dyn ecp_core::analyzer::provider::LanguageProvider>> = needed_names
        .into_par_iter()
        .map(|name| {
            crate::provider_registry::make_provider(name)
                .expect("name sourced from PROVIDER_CONSTRUCTORS")
                .map_err(std::io::Error::other)
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut pipeline = AnalyzerPipeline::new();
    for p in providers {
        pipeline.register_provider(p);
    }

    if prof {
        eprintln!(
            "prof step2.init_providers: {:.2}s",
            t_step2.elapsed().as_secs_f32()
        );
    }
    let t_step3 = std::time::Instant::now();
    // ── Step 3: Analyze files (persistent per-file parse cache) ──────────
    let parse_cache = match parse_cache_root {
        Some(root) if std::env::var_os("ECP_NO_CACHE").is_none() => {
            match crate::parse_cache::ParseCache::open(root) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(
                        "parse_cache: open failed at {:?}: {} — falling back to full parse",
                        root,
                        e
                    );
                    None
                }
            }
        }
        _ => None,
    };
    let cache_ref: Option<&crate::parse_cache::ParseCache> = parse_cache.as_ref();
    let t_parse = std::time::Instant::now();
    let local_graphs = pipeline.analyze_with_cache(files_to_analyze, |_rel_path, hash| {
        cache_ref.and_then(|c| c.get(hash))
    });
    if prof {
        eprintln!(
            "prof step3a.parse_only: {:.2}s",
            t_parse.elapsed().as_secs_f32()
        );
    }
    let t_cache_put = std::time::Instant::now();
    // Write back only fresh parses. Cache hits return the same blob we'd
    // re-serialize on put — the existence stat skips that round-trip for
    // the (~99% on typical commits) hit fraction.
    //
    // CI-A: serialize on the foreground (needs &LocalGraph; can't outlive
    // local_graphs into add_graph), but dispatch the disk writes to a
    // detached background thread. Step 4's build_global_graph (~0.6s on
    // .sample_repo) runs in parallel with these writes; the build_l2 wall
    // drops by ~0.3s on a fresh cache.
    //
    // Each `put` is fsync-free (see `parse_cache::put` doc) so a worker
    // crash or process kill mid-write is recoverable — the corrupt-entry
    // guard in `get()` deletes and reparses on next miss.
    let put_count = if let Some(cache) = cache_ref {
        use rayon::prelude::*;
        let to_write: Vec<(std::path::PathBuf, Vec<u8>)> = local_graphs
            .par_iter()
            .filter(|g| !cache.path_for(&g.content_hash).exists())
            .filter_map(|g| match rkyv::to_bytes::<rkyv::rancor::Error>(g) {
                Ok(bytes) => Some((cache.path_for(&g.content_hash), bytes.into_vec())),
                Err(e) => {
                    tracing::warn!("parse_cache: serialize failed for {:?}: {}", g.file_path, e);
                    None
                }
            })
            .collect();
        let count = to_write.len();
        if !to_write.is_empty() {
            // Sequential disk write on a dedicated thread (NOT a rayon
            // par_iter) so the foreground's pass2_imports_resolve doesn't
            // fight the global rayon pool for CPU. Each
            // atomic_write_bytes_no_fsync goes to pagecache — kernel-buffered,
            // no fsync, so a single thread keeps up. Without this guard, the
            // initial CI-A draft saw pass2 jump from 36ms to 225ms (6×) due
            // to rayon contention.
            std::thread::spawn(move || {
                for (path, bytes) in to_write {
                    if let Err(e) = ecp_core::registry::atomic_write_bytes_no_fsync(&path, &bytes) {
                        tracing::warn!("parse_cache: bg write failed for {:?}: {}", path, e);
                    }
                }
            });
        }
        count
    } else {
        0
    };
    if prof {
        eprintln!(
            "prof step3b.cache_puts_dispatch: {:.2}s ({} puts queued)",
            t_cache_put.elapsed().as_secs_f32(),
            put_count
        );
    }

    if prof {
        eprintln!(
            "prof step3.analyze_files: {:.2}s",
            t_step3.elapsed().as_secs_f32()
        );
    }
    let t_step4 = std::time::Instant::now();
    // ── Step 4: Build global graph ────────────────────────────────────────
    let t_cfg = std::time::Instant::now();
    let aliases = crate::config_parser::parse_configs(src_root);
    let cfg_elapsed = t_cfg.elapsed();
    let t_ingest = std::time::Instant::now();
    let mut builder = GraphBuilder::new()
        .with_path_aliases(aliases)
        .with_repo_root(src_root.to_path_buf())
        .with_resolver_dump(dump_resolver.map(std::path::Path::to_path_buf));
    for graph in local_graphs {
        builder.add_graph(graph);
    }
    let ingest_elapsed = t_ingest.elapsed();
    let t_build = std::time::Instant::now();
    let global_graph = builder.build();
    let build_elapsed = t_build.elapsed();
    let node_count = global_graph.nodes.len();

    if prof {
        eprintln!(
            "prof step4.build_global_graph: {:.2}s ({} nodes) [parse_configs={:.3}s ingest={:.3}s build={:.3}s]",
            t_step4.elapsed().as_secs_f32(),
            node_count,
            cfg_elapsed.as_secs_f32(),
            ingest_elapsed.as_secs_f32(),
            build_elapsed.as_secs_f32(),
        );
    }
    let t_step5 = std::time::Instant::now();
    // ── Step 5: Write graph.bin (atomic) ──────────────────────────────────
    let bin_path = out_dir.join("graph.bin");
    let lock_path = bin_path.with_extension("lock");
    let _lock = ecp_core::registry::FileLock::acquire_exclusive(&lock_path)?;
    let bytes =
        rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph).map_err(std::io::Error::other)?;
    ecp_core::registry::atomic_write_bytes(&bin_path, &bytes)?;

    if prof {
        eprintln!(
            "prof step5.write_graph_bin: {:.2}s ({} bytes)",
            t_step5.elapsed().as_secs_f32(),
            bytes.len()
        );
    }
    // Step 6 (tantivy) has moved to the orchestrator, run AFTER the
    // building → publish_dir rename so the background thread writes to the
    // final reader-visible location. See `orchestrator::build_inside_locked`.
    Ok((node_count, global_graph))
}

pub fn run(args: IndexArgs) -> Result<(), String> {
    // --dump-resolver: produce a resolver-decision JSONL side-output. This is
    // a debug / diff path (consumed by `ecp diff --section bindings` and the
    // oracle harness), NOT a publish. Bypass build_l2 — its same-SHA fast-path
    // attach would skip the analyzer, so the dump would never be produced — and
    // discard the graph; only the JSONL is wanted. ~/.ecp is left untouched.
    let repo_path = std::path::PathBuf::from(&args.repo);
    if let Some(dump_path) = args.dump_resolver {
        let worktree = repo_path;
        if !worktree.exists() {
            return Err(format!("repo path does not exist: {}", worktree.display()));
        }
        let scratch = std::env::temp_dir().join(format!("ecp-dumponly-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).map_err(|e| format!("create scratch dir: {e}"))?;
        let result = run_analyzer_for_paths(&worktree, &scratch, None, Some(&dump_path))
            .map_err(|e| format!("analyzer dump pass: {e}"));
        let _ = std::fs::remove_dir_all(&scratch);
        result?;
        return Ok(());
    }

    let worktree = repo_path;
    if !worktree.exists() {
        return Err(format!("repo path does not exist: {}", worktree.display()));
    }

    let start = std::time::Instant::now();
    let sha = crate::build::orchestrator::head_sha_hex(&worktree)
        .map_err(|e| format!("git rev-parse HEAD: {e}"))?;
    let commit_dir =
        locate_commit_dir(&worktree, &sha).map_err(|e| format!("locate commit dir: {e}"))?;

    match (args.force, commit_dir) {
        (false, Some(existing)) => {
            // Self-heal: if commit_dir was published by a pre-fix binary that
            // wrote per-repo meta but never touched the global registry, an
            // operator re-running `admin index` should still register the
            // repo. Reads per-repo meta then upserts; idempotent on the
            // fixed binary because Registry::upsert_repo skips writes when
            // nothing changes.
            ensure_registry_entry(&worktree).map_err(|e| format!("ensure registry entry: {e}"))?;
            if !args.quiet {
                let st = detect_source_type(&existing);
                eprintln!(
                    "l2.exists sha={} type={:?} elapsed={:.2}s (use --force to rebuild)",
                    &sha[..8.min(sha.len())],
                    st,
                    start.elapsed().as_secs_f32(),
                );
            }
            Ok(())
        }
        (false, None) => {
            let mut r = crate::build::orchestrator::build_l2(&worktree, None)
                .map_err(|e| format!("build_l2 failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.built sha={} type={:?} elapsed={:.2}s",
                    &r.sha_hex[..8.min(r.sha_hex.len())],
                    r.source_type,
                    start.elapsed().as_secs_f32(),
                );
            }
            // CI-M: wait for the background tantivy writer before returning
            // to the shell. The user-visible "l2.built" line was already
            // printed with the foreground wall-clock so the CI-B perf win
            // remains observable; this join only prevents the subprocess
            // from exiting while tantivy still has open temp segments under
            // the publish dir (which raced with TempDir cleanup in tests).
            r.join_background();
            Ok(())
        }
        (true, _) => {
            let mut r = crate::build::force::force_rebuild_l2(&worktree, &sha)
                .map_err(|e| format!("force rebuild failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.rebuilt sha={} type={:?} elapsed={:.2}s l1_kept={} l1_invalidated={} l1_stale_skipped={} l1_meta_reaped={}",
                    &r.sha_hex[..8.min(r.sha_hex.len())],
                    r.source_type,
                    start.elapsed().as_secs_f32(),
                    r.invalidate_report.kept,
                    r.invalidate_report.invalidated,
                    r.invalidate_report.stale_skipped,
                    r.invalidate_report.meta_reaped,
                );
            }
            r.join_background();
            Ok(())
        }
    }
}

/// Read the per-repo meta the build pipeline already published, then upsert
/// the global registry entry for `worktree`. No-op when no per-repo meta
/// exists yet (build pipeline owns first-write; this is the recovery path
/// for already-built commits that pre-date the registry-sync fix).
///
/// Fast path: a lock-free read of `registry.json` short-circuits when the
/// repo is already registered. Every `admin index` re-run on an existing
/// commit hits this — without the early-out we'd pay a flock + per-repo
/// meta read + registry read on a path that's supposed to be near-instant.
fn ensure_registry_entry(worktree: &std::path::Path) -> std::io::Result<()> {
    use ecp_core::registry::{resolve_home_ecp, RegistryFile, RepoAlias, RepoMeta};

    let home_ecp = resolve_home_ecp();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree)?;
    let registry_path = home_ecp.join("registry.json");
    if let Ok(reg) = RegistryFile::read_or_empty(&registry_path) {
        if reg.repos.contains_key(&repo_dir_name) {
            return Ok(());
        }
    }
    let repo_root = home_ecp.join(&repo_dir_name);
    let meta_path = repo_root.join("meta.json");
    if !meta_path.exists() {
        return Ok(());
    }
    let rm = RepoMeta::read(&meta_path)?;
    RegistryFile::upsert_repo_atomic(&home_ecp, RepoAlias::from_repo_meta(repo_dir_name, &rm))
}

fn locate_commit_dir(
    worktree: &std::path::Path,
    sha: &str,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let home_ecp = ecp_core::registry::resolve_home_ecp();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree)?;
    let commits = home_ecp.join(&repo_dir_name).join("commits");
    if !commits.exists() {
        return Ok(None);
    }
    let idx = crate::commit_lookup::CommitIndex::scan(&commits)?;
    let sha_bytes = crate::session::state::sha_hex_to_bytes(sha)
        .ok_or_else(|| std::io::Error::other("invalid sha hex"))?;
    Ok(idx.find(&sha_bytes).map(|name| commits.join(name)))
}

fn detect_source_type(commit_dir: &std::path::Path) -> ecp_core::registry::SourceType {
    ecp_core::registry::CommitBuildMeta::read(&commit_dir.join("meta.json"))
        .map(|m| m.source_type)
        .unwrap_or(ecp_core::registry::SourceType::Commit)
}

/// Walk the scanned file list and collect the registry name of every provider
/// whose files we actually intend to parse. Resolves each
/// path through the canonical `AnalyzerPipeline::provider_name_for_path` so
/// this stays in sync with the single dispatch source of truth instead of
/// re-deriving its own extension table (this repo has twice shipped drift
/// bugs from hand-copied dispatch tables: PR #141/#142, PR #595).
fn detect_needed_providers(
    files: &[(std::path::PathBuf, std::path::PathBuf)],
) -> FxHashSet<&'static str> {
    let mut needed = FxHashSet::default();
    for (path, _) in files {
        // `md`/`txt`/`rst` route to markdown but are outside
        // `provider_name_for_path` (markdown has no path-dispatch entry —
        // see `reanalyze::make_pipeline`'s doc comment); handle here.
        if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("md" | "txt" | "rst")
        ) {
            needed.insert("markdown");
            continue;
        }
        if let Some(name) = AnalyzerPipeline::provider_name_for_path(path) {
            needed.insert(name);
            // `.yaml`/`.yml` route to "kubernetes", which content-gates on
            // `apiVersion:`+`kind:`. The YamlProvider rides along for its
            // document-block path — the one pairing the flag struct encoded.
            if name == "kubernetes" {
                needed.insert("yaml");
            }
        }
    }
    needed
}

fn should_analyze_path(path: &std::path::Path) -> bool {
    // Skip mason brick templates — paths containing `__brick__/` are mustache
    // template source, not real code. Parsing them produces only noise (every
    // brick with the same template structure emits identical symbol names) and
    // contributes ~511 Dart Annotation uid-collisions on the sample corpus.
    // We also reject any path component containing `{{` or `}}` (mason
    // interpolation expressions that appear in the path itself, e.g.
    // `{{name.snakeCase()}}/widget.dart`).
    if path.components().any(|c| {
        if let std::path::Component::Normal(s) = c {
            let s = s.to_string_lossy();
            s == "__brick__" || s.contains("{{") || s.contains("}}")
        } else {
            false
        }
    }) {
        return false;
    }

    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if matches!(file_name, "Dockerfile" | "dockerfile") {
        return true;
    }
    if matches!(
        file_name,
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
    ) {
        return true;
    }
    if is_github_actions_workflow(path) {
        return true;
    }
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some(
            "ts" | "tsx"
                | "py"
                | "pyi"
                | "go"
                | "rs"
                | "java"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "php"
                | "rb"
                | "kt"
                | "kts"
                | "cs"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "cc"
                | "hh"
                | "cxx"
                | "hxx"
                | "swift"
                | "dart"
                | "md"
                | "txt"
                | "rst"
                | "sh"
                | "bash"
                | "lua"
                | "luau"
                | "cr"
                | "sol"
                | "move"
                | "dockerfile"
                | "nim"
                | "tf"
                | "tfvars"
                | "hcl"
                | "vy"
                | "sql"
                | "cairo"
                | "v"
                | "sv"
                | "vh"
                | "svh"
                | "zig"
                | "vue"
                | "astro"
                | "svelte"
                | "proto"
                | "json"
                // `.yaml`/`.yml` are scanned so k8s manifests reach the
                // content-sniffing Kubernetes provider; non-k8s YAML is gated
                // out cheaply by the provider, not here.
                | "yml"
                | "yaml"
        )
    )
}

fn is_github_actions_workflow(path: &std::path::Path) -> bool {
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "yml" | "yaml"))
    {
        return false;
    }

    let mut prev_is_github = false;
    for component in path.components() {
        if prev_is_github && component.as_os_str() == "workflows" {
            return true;
        }
        prev_is_github = component.as_os_str() == ".github";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::should_analyze_path;
    use std::path::Path;

    #[test]
    fn skips_mason_brick_template_dir() {
        assert!(!should_analyze_path(Path::new(
            "bricks/feature/__brick__/lib/widget.dart"
        )));
    }

    #[test]
    fn skips_mustache_interpolated_path_component() {
        assert!(!should_analyze_path(Path::new(
            "bricks/feature/{{name.snakeCase()}}/widget.dart"
        )));
    }

    #[test]
    fn keeps_real_dart_path() {
        assert!(should_analyze_path(Path::new("lib/main.dart")));
    }

    /// Every plain extension the canonical `EXTENSION_TABLE` routes must
    /// produce a `NeededProviders` flag `detect_needed_providers` can derive
    /// (a `set_needed_flag` arm exists for its provider name), pinning the
    /// three-table sync invariant this refactor introduces. Exhaustively
    /// covers the extension list the pre-refactor `detect_needed_providers`
    /// hand-duplicated, so a dropped extension here fails loudly instead of
    /// silently (this repo shipped that exact bug for `.yaml` in PR #595).
    #[test]
    fn detect_needed_providers_test_every_extension_table_entry_sets_a_flag() {
        for (ext, name) in ecp_core::analyzer::pipeline::AnalyzerPipeline::EXTENSION_TABLE {
            let path = std::path::PathBuf::from(format!("f.{ext}"));
            let needed = super::detect_needed_providers(&[(path.clone(), path)]);
            assert!(
                needed.contains(name),
                "extension {ext:?} routes to provider {name:?} but no NeededProviders flag is set"
            );
        }
    }

    /// The special-case path dispatches (Dockerfile, docker-compose, GitHub
    /// Actions workflow, generic `.yaml`/`.yml` → kubernetes) must also set
    /// their `NeededProviders` flag — these aren't in `EXTENSION_TABLE` so
    /// the test above doesn't cover them.
    #[test]
    fn detect_needed_providers_test_path_specials_set_flags() {
        let cases: &[(&str, &str)] = &[
            ("Dockerfile", "dockerfile"),
            ("docker-compose.yml", "docker-compose"),
            (".github/workflows/ci.yml", "github-actions"),
            ("k8s/deployment.yaml", "kubernetes"),
        ];
        for (rel, name) in cases {
            let path = std::path::PathBuf::from(rel);
            let needed = super::detect_needed_providers(&[(path.clone(), path)]);
            assert!(
                needed.contains(name),
                "path {rel:?} should set the {name:?} NeededProviders flag"
            );
        }
    }

    /// `md`/`txt`/`rst` route to markdown outside `provider_name_for_path`
    /// (see `detect_needed_providers`'s doc comment) — pin that special case
    /// too since it isn't covered by the extension-table sweep above.
    #[test]
    fn detect_needed_providers_test_markdown_extensions_set_flag() {
        for ext in ["md", "txt", "rst"] {
            let path = std::path::PathBuf::from(format!("f.{ext}"));
            let needed = super::detect_needed_providers(&[(path.clone(), path)]);
            assert!(
                needed.contains("markdown"),
                "extension {ext:?} should select the markdown provider"
            );
        }
    }

    /// A generic `.yaml`/`.yml` selects TWO providers: `kubernetes` (which
    /// content-gates on `apiVersion:`+`kind:`) and `yaml` (its document-block
    /// path). The pairing is the one special case `detect_needed_providers`
    /// carries by hand, so it needs a test of its own — the extension-table
    /// sweep never reaches it, since no extension maps to the name `"yaml"`.
    #[test]
    fn detect_needed_providers_test_generic_yaml_selects_kubernetes_and_yaml() {
        for rel in ["k8s/deployment.yaml", "config/app.yml"] {
            let path = std::path::PathBuf::from(rel);
            let needed = super::detect_needed_providers(&[(path.clone(), path)]);
            assert!(
                needed.contains("kubernetes") && needed.contains("yaml"),
                "{rel:?} must select both kubernetes and yaml, got {needed:?}"
            );
        }
    }

    /// Every name the canonical dispatch can produce must be constructible —
    /// the set carries names straight from `provider_name_for_path` to
    /// `PROVIDER_CONSTRUCTORS`, so a name with no constructor is a silently
    /// skipped language.
    #[test]
    fn detect_needed_providers_test_every_selected_name_is_constructible() {
        let known: std::collections::HashSet<&str> =
            crate::provider_registry::PROVIDER_CONSTRUCTORS
                .iter()
                .map(|(name, _)| *name)
                .collect();
        for (ext, _) in ecp_core::analyzer::pipeline::AnalyzerPipeline::EXTENSION_TABLE {
            let path = std::path::PathBuf::from(format!("f.{ext}"));
            for name in super::detect_needed_providers(&[(path.clone(), path)]) {
                assert!(
                    known.contains(name),
                    "extension {ext:?} selects provider {name:?}, which has no constructor"
                );
            }
        }
    }
}
