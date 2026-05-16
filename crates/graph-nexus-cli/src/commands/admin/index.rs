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

    /// Force full analysis even if the graph seems up to date.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Optional path to write a JSONL dump of every resolver decision.
    /// Used by the oracle verification harness; off by default.
    /// Spec: docs/specs/2026-05-15-resolver-oracle-harness.md
    #[arg(long)]
    pub dump_resolver: Option<std::path::PathBuf>,

    /// Bypass the incremental parse cache and force a full re-parse of
    /// every file. Also honored via `GNX_NO_CACHE=1`. Use when you suspect
    /// the cache has gone stale in a way the binary fingerprint didn't
    /// catch (e.g. external grammar update from outside the build) or
    /// for benchmark baselines.
    #[arg(long, default_value_t = false)]
    pub no_cache: bool,

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
    // ── Step 1: Scan files ────────────────────────────────────────────────
    let mut files_to_analyze: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    let mut skipped_large_files: u64 = 0;
    const MAX_FILE_SIZE: u64 = 512 * 1024; // 512 KB

    let walker = WalkBuilder::new(src_root).hidden(false).build();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.len() > MAX_FILE_SIZE {
                            skipped_large_files += 1;
                            continue;
                        }
                    }

                    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    let is_dockerfile_basename = matches!(file_name, "Dockerfile" | "dockerfile");
                    let is_compose_basename = matches!(
                        file_name,
                        "docker-compose.yml"
                            | "docker-compose.yaml"
                            | "compose.yml"
                            | "compose.yaml"
                    );
                    let is_gha_workflow = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| matches!(e, "yml" | "yaml"))
                        && {
                            let components: Vec<_> = path.components().collect();
                            components.windows(2).any(|w| {
                                w[0].as_os_str() == ".github" && w[1].as_os_str() == "workflows"
                            })
                        };
                    if is_dockerfile_basename || is_compose_basename || is_gha_workflow {
                        let rel_path = path.strip_prefix(src_root).unwrap_or(path);
                        files_to_analyze.push((path.to_path_buf(), rel_path.to_path_buf()));
                    } else if let Some(
                        "ts" | "tsx" | "py" | "pyi" | "go" | "rs" | "java" | "js" | "jsx" | "mjs"
                        | "cjs" | "php" | "rb" | "kt" | "kts" | "cs" | "c" | "h" | "cpp" | "hpp"
                        | "cc" | "hh" | "cxx" | "hxx" | "swift" | "dart" | "md" | "txt" | "rst"
                        | "sh" | "bash" | "lua" | "luau" | "cr" | "sol" | "move" | "dockerfile"
                        | "nim" | "tf" | "tfvars" | "hcl" | "vy" | "sql" | "cairo" | "v" | "sv"
                        | "vh" | "svh" | "zig",
                    ) = path.extension().and_then(|s| s.to_str())
                    {
                        let rel_path = path.strip_prefix(src_root).unwrap_or(path);
                        files_to_analyze.push((path.to_path_buf(), rel_path.to_path_buf()));
                    }
                }
            }
            Err(err) => {
                tracing::warn!("Error accessing path during scan: {}", err);
            }
        }
    }

    if skipped_large_files > 0 {
        tracing::warn!(
            "Skipped {} files > 512 KB during analysis of {:?}",
            skipped_large_files,
            src_root
        );
    }

    // ── Step 2: Initialize pipeline with only needed providers ────────────
    let needed = detect_needed_providers(&files_to_analyze);
    let mut pipeline = AnalyzerPipeline::new();
    if needed.typescript {
        pipeline.register_provider(Box::new(
            TypeScriptProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.python {
        pipeline.register_provider(Box::new(
            PythonProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.go {
        pipeline.register_provider(Box::new(GoProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.rust {
        pipeline.register_provider(Box::new(
            RustProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.java {
        pipeline.register_provider(Box::new(
            JavaProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.javascript {
        pipeline.register_provider(Box::new(
            JavaScriptProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.php {
        pipeline.register_provider(Box::new(PhpProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.ruby {
        pipeline.register_provider(Box::new(
            RubyProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.kotlin {
        pipeline.register_provider(Box::new(
            KotlinProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.csharp {
        pipeline.register_provider(Box::new(
            CSharpProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.c {
        pipeline.register_provider(Box::new(CProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.cpp {
        pipeline.register_provider(Box::new(CppProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.swift {
        pipeline.register_provider(Box::new(SwiftProvider::new().unwrap()));
    }
    if needed.dart {
        pipeline.register_provider(Box::new(
            DartProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.markdown {
        pipeline.register_provider(Box::new(
            MarkdownProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.yaml {
        pipeline.register_provider(Box::new(
            YamlProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.github_actions {
        pipeline.register_provider(Box::new(
            GitHubActionsProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.bash {
        pipeline.register_provider(Box::new(
            BashProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.lua {
        pipeline.register_provider(Box::new(LuaProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.crystal {
        pipeline.register_provider(Box::new(
            CrystalProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.move_lang {
        pipeline.register_provider(Box::new(
            MoveProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.solidity {
        pipeline.register_provider(Box::new(
            SolidityProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.dockerfile {
        pipeline.register_provider(Box::new(
            DockerfileProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.nim {
        pipeline.register_provider(Box::new(NimProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.hcl {
        pipeline.register_provider(Box::new(HclProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.sql {
        pipeline.register_provider(Box::new(SqlProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.vyper {
        pipeline.register_provider(Box::new(
            VyperProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.verilog {
        pipeline.register_provider(Box::new(
            VerilogProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.cairo {
        pipeline.register_provider(Box::new(
            CairoProvider::new().map_err(std::io::Error::other)?,
        ));
    }
    if needed.zig {
        pipeline.register_provider(Box::new(ZigProvider::new().map_err(std::io::Error::other)?));
    }
    if needed.docker_compose {
        pipeline.register_provider(Box::new(
            DockerComposeProvider::new().map_err(std::io::Error::other)?,
        ));
    }

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
    let local_graphs = pipeline.analyze_with_cache(files_to_analyze, |_rel_path, hash| {
        cache_ref.and_then(|c| c.get(hash))
    });
    // Write back only fresh parses. Cache hits return the same blob we'd
    // re-serialize on put — the existence stat skips that round-trip for
    // the (~99% on typical commits) hit fraction.
    if let Some(cache) = cache_ref {
        for g in &local_graphs {
            if !cache.path_for(&g.content_hash).exists() {
                if let Err(e) = cache.put(g) {
                    tracing::warn!(
                        "parse_cache: put failed for {:?}: {}",
                        g.file_path,
                        e
                    );
                }
            }
        }
    }

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

    // ── Step 5: Write graph.bin (atomic) ──────────────────────────────────
    let bin_path = out_dir.join("graph.bin");
    let lock_path = bin_path.with_extension("lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)?;
    let bytes =
        rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph).map_err(std::io::Error::other)?;
    graph_nexus_core::registry::atomic_write_bytes(&bin_path, &bytes)?;

    // ── Step 6: Build tantivy full-text index (best-effort) ───────────────
    if let Err(e) = crate::search::TantivyEngine::build_index(out_dir, &global_graph) {
        tracing::warn!(
            "Full-text index build failed for {:?}: {}; exact-name queries still work",
            out_dir,
            e
        );
    }

    Ok(node_count)
}

pub fn run(args: IndexArgs) -> Result<(), String> {
    if args.force || args.no_cache || args.dump_resolver.is_some() {
        eprintln!(
            "warning: --force / --no-cache / --dump-resolver \
             flags accepted but currently no-op in v2 layout; will be wired in Phase 5+"
        );
    }

    let worktree = std::path::PathBuf::from(&args.repo);
    if !worktree.exists() {
        return Err(format!("repo path does not exist: {}", worktree.display()));
    }

    let start = std::time::Instant::now();
    let result = crate::build::orchestrator::build_l2(&worktree, None)
        .map_err(|e| format!("build_l2 failed: {e}"))?;

    if !args.quiet {
        let elapsed = start.elapsed().as_secs_f32();
        eprintln!("l2.built sha={} type={:?} elapsed={:.2}s", &result.sha_hex[..8], result.source_type, elapsed);
    }
    Ok(())
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
        let is_gha = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "yml" | "yaml"))
            && {
                let components: Vec<_> = path.components().collect();
                components
                    .windows(2)
                    .any(|w| w[0].as_os_str() == ".github" && w[1].as_os_str() == "workflows")
            };
        if is_gha {
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
