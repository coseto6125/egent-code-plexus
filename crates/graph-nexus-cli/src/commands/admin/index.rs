use clap::Args;
use graph_nexus_analyzer::resolution::builder::GraphBuilder;
use graph_nexus_analyzer::{
    bash::parser::BashProvider, c::parser::CProvider, c_sharp::parser::CSharpProvider,
    cairo::parser::CairoProvider, cpp::parser::CppProvider, crystal::parser::CrystalProvider,
    dart::parser::DartProvider, docker_compose::parser::DockerComposeProvider,
    dockerfile::parser::DockerfileProvider, github_actions::parser::GitHubActionsProvider,
    go::parser::GoProvider, hcl::parser::HclProvider, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider,
    lua::parser::LuaProvider, markdown::parser::MarkdownProvider, move_lang::parser::MoveProvider,
    nim::parser::NimProvider, php::parser::PhpProvider, python::parser::PythonProvider,
    ruby::parser::RubyProvider, rust::parser::RustProvider, solidity::parser::SolidityProvider,
    sql::parser::SqlProvider, swift::parser::SwiftProvider, typescript::parser::TypeScriptProvider,
    verilog::parser::VerilogProvider, vyper::parser::VyperProvider, yaml::parser::YamlProvider,
    zig::parser::ZigProvider,
};
use graph_nexus_core::analyzer::pipeline::AnalyzerPipeline;
use ignore::WalkBuilder;

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
/// when env `GNX_NO_CACHE=1` is set — matches `--no-cache` flag semantics.
///
/// Returns the number of nodes written to `graph.bin`.
pub fn run_analyzer_for_paths(
    src_root: &std::path::Path,
    out_dir: &std::path::Path,
    parse_cache_root: Option<&std::path::Path>,
) -> std::io::Result<usize> {
    let prof = std::env::var("GNX_PROF").is_ok();
    let t_step1 = std::time::Instant::now();
    // ── Step 1: Scan files (parallel walker) ──────────────────────────────
    // `WalkBuilder::build_parallel()` fans the directory traversal across
    // rayon-style worker threads. Each visitor pushes accepted paths into
    // an MPSC channel; the main thread drains into the analysis list.
    // Sequential `.build()` was ~100ms on .sample_repo's 22.7k entries —
    // parallel cuts the syscall-bound stat/readdir tax.
    const MAX_FILE_SIZE: u64 = 512 * 1024; // 512 KB
    let (file_tx, file_rx) =
        std::sync::mpsc::channel::<(std::path::PathBuf, std::path::PathBuf)>();
    let skipped_large = std::sync::atomic::AtomicU64::new(0);
    let skipped_large_ref = &skipped_large;
    let src_root_ref = src_root;
    WalkBuilder::new(src_root).hidden(false).build_parallel().run(|| {
        let tx = file_tx.clone();
        Box::new(move |result| {
            if let Ok(entry) = result {
                let path = entry.path();
                if path.is_file() && should_analyze_path(path) {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.len() > MAX_FILE_SIZE {
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
            "Skipped {} files > 512 KB during analysis of {:?}",
            skipped_large_files,
            src_root
        );
    }

    if prof { eprintln!("prof step1.scan_files: {:.2}s ({} files)", t_step1.elapsed().as_secs_f32(), files_to_analyze.len()); }
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
    type ProviderFactory =
        Box<dyn FnOnce() -> std::io::Result<Box<dyn graph_nexus_core::analyzer::provider::LanguageProvider>> + Send>;
    let mut factories: Vec<ProviderFactory> = Vec::new();
    macro_rules! add {
        ($flag:expr, $ctor:expr) => {
            if $flag { factories.push(Box::new(|| {
                $ctor.map(|p| Box::new(p) as Box<dyn graph_nexus_core::analyzer::provider::LanguageProvider>)
                    .map_err(std::io::Error::other)
            })); }
        };
    }
    add!(needed.typescript, TypeScriptProvider::new());
    add!(needed.python, PythonProvider::new());
    add!(needed.go, GoProvider::new());
    add!(needed.rust, RustProvider::new());
    add!(needed.java, JavaProvider::new());
    add!(needed.javascript, JavaScriptProvider::new());
    add!(needed.php, PhpProvider::new());
    add!(needed.ruby, RubyProvider::new());
    add!(needed.kotlin, KotlinProvider::new());
    add!(needed.csharp, CSharpProvider::new());
    add!(needed.c, CProvider::new());
    add!(needed.cpp, CppProvider::new());
    if needed.swift {
        // SwiftProvider::new returns anyhow::Result that the original site
        // unwrapped — preserve that contract.
        factories.push(Box::new(|| {
            SwiftProvider::new()
                .map(|p| Box::new(p) as Box<dyn graph_nexus_core::analyzer::provider::LanguageProvider>)
                .map_err(std::io::Error::other)
        }));
    }
    add!(needed.dart, DartProvider::new());
    add!(needed.markdown, MarkdownProvider::new());
    add!(needed.yaml, YamlProvider::new());
    add!(needed.github_actions, GitHubActionsProvider::new());
    add!(needed.bash, BashProvider::new());
    add!(needed.lua, LuaProvider::new());
    add!(needed.crystal, CrystalProvider::new());
    add!(needed.move_lang, MoveProvider::new());
    add!(needed.solidity, SolidityProvider::new());
    add!(needed.dockerfile, DockerfileProvider::new());
    add!(needed.nim, NimProvider::new());
    add!(needed.hcl, HclProvider::new());
    add!(needed.sql, SqlProvider::new());
    add!(needed.vyper, VyperProvider::new());
    add!(needed.verilog, VerilogProvider::new());
    add!(needed.cairo, CairoProvider::new());
    add!(needed.zig, ZigProvider::new());
    add!(needed.docker_compose, DockerComposeProvider::new());

    use rayon::prelude::*;
    let providers: Vec<Box<dyn graph_nexus_core::analyzer::provider::LanguageProvider>> = factories
        .into_par_iter()
        .map(|f| f())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut pipeline = AnalyzerPipeline::new();
    for p in providers { pipeline.register_provider(p); }

    if prof { eprintln!("prof step2.init_providers: {:.2}s", t_step2.elapsed().as_secs_f32()); }
    let t_step3 = std::time::Instant::now();
    // ── Step 3: Analyze files (persistent per-file parse cache) ──────────
    let parse_cache = match parse_cache_root {
        Some(root) if std::env::var_os("GNX_NO_CACHE").is_none() => {
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
    if prof { eprintln!("prof step3a.parse_only: {:.2}s", t_parse.elapsed().as_secs_f32()); }
    let t_cache_put = std::time::Instant::now();
    // Write back only fresh parses. Cache hits return the same blob we'd
    // re-serialize on put — the existence stat skips that round-trip for
    // the (~99% on typical commits) hit fraction. Parallelize via
    // `par_iter` (rayon picks `num_cpus` workers — no hardcoded thread
    // count). Each `put` is now fsync-free (see `parse_cache::put` doc),
    // so the workers don't serialize on disk-sync syscalls.
    let put_count = if let Some(cache) = cache_ref {
        use rayon::prelude::*;
        local_graphs
            .par_iter()
            .filter(|g| !cache.path_for(&g.content_hash).exists())
            .map(|g| {
                if let Err(e) = cache.put(g) {
                    tracing::warn!(
                        "parse_cache: put failed for {:?}: {}",
                        g.file_path,
                        e
                    );
                }
            })
            .count()
    } else {
        0
    };
    if prof { eprintln!("prof step3b.cache_puts: {:.2}s ({} puts)", t_cache_put.elapsed().as_secs_f32(), put_count); }

    if prof { eprintln!("prof step3.analyze_files: {:.2}s", t_step3.elapsed().as_secs_f32()); }
    let t_step4 = std::time::Instant::now();
    // ── Step 4: Build global graph ────────────────────────────────────────
    let aliases = crate::config_parser::parse_configs(src_root);
    let mut builder = GraphBuilder::new()
        .with_path_aliases(aliases)
        .with_repo_root(src_root.to_path_buf());
    for graph in local_graphs {
        builder.add_graph(graph);
    }
    let global_graph = builder.build();
    let node_count = global_graph.nodes.len();

    if prof { eprintln!("prof step4.build_global_graph: {:.2}s ({} nodes)", t_step4.elapsed().as_secs_f32(), node_count); }
    let t_step5 = std::time::Instant::now();
    // ── Step 5: Write graph.bin (atomic) ──────────────────────────────────
    let bin_path = out_dir.join("graph.bin");
    let lock_path = bin_path.with_extension("lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)?;
    let bytes =
        rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph).map_err(std::io::Error::other)?;
    graph_nexus_core::registry::atomic_write_bytes(&bin_path, &bytes)?;

    if prof { eprintln!("prof step5.write_graph_bin: {:.2}s ({} bytes)", t_step5.elapsed().as_secs_f32(), bytes.len()); }
    let t_step6 = std::time::Instant::now();
    // ── Step 6: Build tantivy full-text index (best-effort) ───────────────
    if let Err(e) = crate::search::TantivyEngine::build_index(out_dir, &global_graph) {
        tracing::warn!(
            "Full-text index build failed for {:?}: {}; exact-name queries still work",
            out_dir,
            e
        );
    }

    if prof { eprintln!("prof step6.tantivy: {:.2}s", t_step6.elapsed().as_secs_f32()); }
    Ok(node_count)
}

pub fn run(args: IndexArgs) -> Result<(), String> {
    if args.dump_resolver.is_some() {
        eprintln!(
            "warning: --dump-resolver accepted but not yet wired in v2 layout; \
             will be re-wired alongside `gnx diff` baseline path"
        );
    }

    let worktree = std::path::PathBuf::from(&args.repo);
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
            ensure_registry_entry(&worktree)
                .map_err(|e| format!("ensure registry entry: {e}"))?;
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
            let r = crate::build::orchestrator::build_l2(&worktree, None)
                .map_err(|e| format!("build_l2 failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.built sha={} type={:?} elapsed={:.2}s",
                    &r.sha_hex[..8.min(r.sha_hex.len())],
                    r.source_type,
                    start.elapsed().as_secs_f32(),
                );
            }
            Ok(())
        }
        (true, _) => {
            let r = crate::build::force::force_rebuild_l2(&worktree, &sha)
                .map_err(|e| format!("force rebuild failed: {e}"))?;
            if !args.quiet {
                eprintln!(
                    "l2.rebuilt sha={} type={:?} elapsed={:.2}s l1_kept={} l1_invalidated={} l1_stale_skipped={}",
                    &r.sha_hex[..8.min(r.sha_hex.len())],
                    r.source_type,
                    start.elapsed().as_secs_f32(),
                    r.invalidate_report.kept,
                    r.invalidate_report.invalidated,
                    r.invalidate_report.stale_skipped,
                );
            }
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
    use graph_nexus_core::registry::{resolve_home_gnx, RegistryFile, RepoAlias, RepoMeta};

    let home_gnx = resolve_home_gnx();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree)?;
    let registry_path = home_gnx.join("registry.json");
    if let Ok(reg) = RegistryFile::read_or_empty(&registry_path) {
        if reg.repos.contains_key(&repo_dir_name) {
            return Ok(());
        }
    }
    let repo_root = home_gnx.join(&repo_dir_name);
    let meta_path = repo_root.join("meta.json");
    if !meta_path.exists() {
        return Ok(());
    }
    let rm = RepoMeta::read(&meta_path)?;
    RegistryFile::upsert_repo_atomic(&home_gnx, RepoAlias::from_repo_meta(repo_dir_name, &rm))
}

fn locate_commit_dir(
    worktree: &std::path::Path,
    sha: &str,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let repo_dir_name = crate::repo_identity::repo_dir_name_for_cwd(worktree)?;
    let commits = home_gnx.join(&repo_dir_name).join("commits");
    if !commits.exists() {
        return Ok(None);
    }
    let idx = crate::commit_lookup::CommitIndex::scan(&commits)?;
    let sha_bytes = crate::session::state::sha_hex_to_bytes(sha)
        .ok_or_else(|| std::io::Error::other("invalid sha hex"))?;
    Ok(idx.find(&sha_bytes).map(|name| commits.join(name)))
}

fn detect_source_type(commit_dir: &std::path::Path) -> graph_nexus_core::registry::SourceType {
    graph_nexus_core::registry::CommitBuildMeta::read(&commit_dir.join("meta.json"))
        .map(|m| m.source_type)
        .unwrap_or(graph_nexus_core::registry::SourceType::Commit)
}

#[derive(Default)]
struct NeededProviders {
    typescript: bool,
    python: bool,
    go: bool,
    rust: bool,
    java: bool,
    javascript: bool,
    php: bool,
    ruby: bool,
    kotlin: bool,
    csharp: bool,
    c: bool,
    cpp: bool,
    swift: bool,
    dart: bool,
    markdown: bool,
    yaml: bool,
    github_actions: bool,
    bash: bool,
    lua: bool,
    crystal: bool,
    move_lang: bool,
    solidity: bool,
    dockerfile: bool,
    nim: bool,
    hcl: bool,
    sql: bool,
    vyper: bool,
    verilog: bool,
    cairo: bool,
    zig: bool,
    docker_compose: bool,
}

/// Walk the scanned file list, set the flag for each language whose files we
/// actually intend to parse. Returning a struct instead of a `HashSet<&str>`
/// keeps the `if needed.X` call-sites obvious in the caller and avoids
/// stringly-typed lookups at every register_provider step.
fn detect_needed_providers(files: &[(std::path::PathBuf, std::path::PathBuf)]) -> NeededProviders {
    let mut n = NeededProviders::default();
    for (path, _) in files {
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(file_name, "Dockerfile" | "dockerfile") {
            n.dockerfile = true;
            continue;
        }
        if matches!(
            file_name,
            "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
        ) {
            n.docker_compose = true;
            continue;
        }
        if is_github_actions_workflow(path) {
            n.github_actions = true;
            continue;
        }
        match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
            "ts" | "tsx" => n.typescript = true,
            "py" | "pyi" => n.python = true,
            "go" => n.go = true,
            "rs" => n.rust = true,
            "java" => n.java = true,
            "js" | "jsx" | "mjs" | "cjs" => n.javascript = true,
            "php" => n.php = true,
            "rb" => n.ruby = true,
            "kt" | "kts" => n.kotlin = true,
            "cs" => n.csharp = true,
            "c" | "h" => n.c = true,
            "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" => n.cpp = true,
            "swift" => n.swift = true,
            "dart" => n.dart = true,
            "md" | "txt" | "rst" => n.markdown = true,
            "sh" | "bash" => n.bash = true,
            "lua" | "luau" => n.lua = true,
            "cr" => n.crystal = true,
            "move" => n.move_lang = true,
            "sol" => n.solidity = true,
            "dockerfile" => n.dockerfile = true,
            "nim" => n.nim = true,
            "tf" | "tfvars" | "hcl" => n.hcl = true,
            "sql" => n.sql = true,
            "vy" => n.vyper = true,
            "v" | "sv" | "vh" | "svh" => n.verilog = true,
            "cairo" => n.cairo = true,
            "zig" => n.zig = true,
            "yml" | "yaml" => n.yaml = true,
            _ => {}
        }
    }
    n
}

fn should_analyze_path(path: &std::path::Path) -> bool {
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
            "ts" | "tsx" | "py" | "pyi" | "go" | "rs" | "java" | "js" | "jsx" | "mjs"
            | "cjs" | "php" | "rb" | "kt" | "kts" | "cs" | "c" | "h" | "cpp" | "hpp"
            | "cc" | "hh" | "cxx" | "hxx" | "swift" | "dart" | "md" | "txt" | "rst"
            | "sh" | "bash" | "lua" | "luau" | "cr" | "sol" | "move" | "dockerfile"
            | "nim" | "tf" | "tfvars" | "hcl" | "vy" | "sql" | "cairo" | "v" | "sv"
            | "vh" | "svh" | "zig"
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
