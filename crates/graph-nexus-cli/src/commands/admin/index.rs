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
    sql::parser::SqlProvider, typescript::parser::TypeScriptProvider,
    verilog::parser::VerilogProvider, vyper::parser::VyperProvider, yaml::parser::YamlProvider,
    zig::parser::ZigProvider,
};
use graph_nexus_core::analyzer::pipeline::AnalyzerPipeline;
use ignore::WalkBuilder;
use std::time::Instant;

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    #[arg(long)]
    pub repo: String,

    #[arg(long, default_value_t = false)]
    pub embeddings: bool,

    /// Drop the existing embeddings cache entirely during analysis.
    #[arg(long, default_value_t = false)]
    pub drop_embeddings: bool,

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

pub fn run(args: IndexArgs) -> Result<(), String> {
    let start_time = Instant::now();
    let repo_path = std::path::PathBuf::from(&args.repo);

    if !repo_path.exists() || !repo_path.is_dir() {
        return Err(format!(
            "Repository path {:?} does not exist or is not a directory",
            repo_path
        ));
    }

    // Step 1: Scan files
    let scan_start = Instant::now();
    let mut files_to_analyze = Vec::new();
    let mut skipped_large_files = 0;
    const MAX_FILE_SIZE: u64 = 512 * 1024; // 512 KB

    let walker = WalkBuilder::new(&repo_path).hidden(false).build();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
                    // Layer 2: File size limit (spec §1.10)
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.len() > MAX_FILE_SIZE {
                            skipped_large_files += 1;
                            continue;
                        }
                    }

                    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    // Extension-less Dockerfile variants: check basename before extension.
                    let is_dockerfile_basename = matches!(file_name, "Dockerfile" | "dockerfile");
                    let is_compose_basename = matches!(
                        file_name,
                        "docker-compose.yml"
                            | "docker-compose.yaml"
                            | "compose.yml"
                            | "compose.yaml"
                    );
                    // GitHub Actions: path-based routing for .github/workflows/*.yml|yaml
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
                        let rel_path = path.strip_prefix(&repo_path).unwrap_or(path);
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
                        let rel_path = path.strip_prefix(&repo_path).unwrap_or(path);
                        files_to_analyze.push((path.to_path_buf(), rel_path.to_path_buf()));
                    }
                }
            }
            Err(err) => {
                tracing::warn!("Error accessing path during scan: {}", err);
            }
        }
    }
    let scan_duration = scan_start.elapsed();

    let state =
        crate::git_state::resolve(&repo_path).map_err(|e| format!("git_state resolve: {e}"))?;

    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();

    let existing_repos: Vec<(String, String)> = {
        let reg = graph_nexus_core::registry::Registry::open(&home_gnx)
            .map_err(|e| format!("registry open: {e}"))?;
        reg.snapshot()
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.worktree_path.clone()))
            .collect()
    };
    let layout = graph_nexus_core::registry::IndexLayout::resolve(
        &home_gnx,
        &state.repo_name,
        &state.branch,
        state.worktree_path.to_string_lossy().as_ref(),
        &existing_repos,
    )
    .map_err(|e| format!("layout resolve: {e}"))?;
    std::fs::create_dir_all(&layout.index_dir)
        .map_err(|e| format!("Failed to create index dir: {e}"))?;

    let bin_path = layout.index_dir.join("graph.bin");
    let meta_path = layout.index_dir.join("meta.json");
    let embeddings_flag = args.embeddings;

    // Step 2: Initialize pipeline and register only the providers needed for
    // files we actually scanned. tree-sitter language load + query compile is
    // ~10–20ms per provider × 30 providers ≈ 0.5s of per-invocation overhead;
    // small fixtures (single-language tests, snippet repos) end up registering
    // 1–3 providers instead of all 30.
    let needed = detect_needed_providers(&files_to_analyze);
    let analyze_start = Instant::now();
    let mut pipeline = AnalyzerPipeline::new();
    if needed.typescript {
        pipeline.register_provider(Box::new(TypeScriptProvider::new().unwrap()));
    }
    if needed.python {
        pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
    }
    if needed.go {
        pipeline.register_provider(Box::new(GoProvider::new().unwrap()));
    }
    if needed.rust {
        pipeline.register_provider(Box::new(RustProvider::new().unwrap()));
    }
    if needed.java {
        pipeline.register_provider(Box::new(JavaProvider::new().unwrap()));
    }
    if needed.javascript {
        pipeline.register_provider(Box::new(JavaScriptProvider::new().unwrap()));
    }
    if needed.php {
        pipeline.register_provider(Box::new(PhpProvider::new().unwrap()));
    }
    if needed.ruby {
        pipeline.register_provider(Box::new(RubyProvider::new().unwrap()));
    }
    if needed.kotlin {
        pipeline.register_provider(Box::new(KotlinProvider::new().unwrap()));
    }
    if needed.csharp {
        pipeline.register_provider(Box::new(CSharpProvider::new().unwrap()));
    }
    if needed.c {
        pipeline.register_provider(Box::new(CProvider::new().unwrap()));
    }
    if needed.cpp {
        pipeline.register_provider(Box::new(CppProvider::new().unwrap()));
    }
    if needed.dart {
        pipeline.register_provider(Box::new(DartProvider::new().unwrap()));
    }
    if needed.markdown {
        pipeline.register_provider(Box::new(MarkdownProvider::new().unwrap()));
    }
    if needed.yaml {
        pipeline.register_provider(Box::new(YamlProvider::new().unwrap()));
    }
    if needed.github_actions {
        pipeline.register_provider(Box::new(GitHubActionsProvider::new().unwrap()));
    }
    if needed.bash {
        pipeline.register_provider(Box::new(BashProvider::new().unwrap()));
    }
    if needed.lua {
        pipeline.register_provider(Box::new(LuaProvider::new().unwrap()));
    }
    if needed.crystal {
        pipeline.register_provider(Box::new(CrystalProvider::new().unwrap()));
    }
    if needed.move_lang {
        pipeline.register_provider(Box::new(MoveProvider::new().unwrap()));
    }
    if needed.solidity {
        pipeline.register_provider(Box::new(SolidityProvider::new().unwrap()));
    }
    if needed.dockerfile {
        pipeline.register_provider(Box::new(DockerfileProvider::new().unwrap()));
    }
    if needed.nim {
        pipeline.register_provider(Box::new(NimProvider::new().unwrap()));
    }
    if needed.hcl {
        pipeline.register_provider(Box::new(HclProvider::new().unwrap()));
    }
    if needed.sql {
        pipeline.register_provider(Box::new(SqlProvider::new().unwrap()));
    }
    if needed.vyper {
        pipeline.register_provider(Box::new(VyperProvider::new().unwrap()));
    }
    if needed.verilog {
        pipeline.register_provider(Box::new(VerilogProvider::new().unwrap()));
    }
    if needed.cairo {
        pipeline.register_provider(Box::new(CairoProvider::new().unwrap()));
    }
    if needed.zig {
        pipeline.register_provider(Box::new(ZigProvider::new().unwrap()));
    }
    if needed.docker_compose {
        pipeline.register_provider(Box::new(DockerComposeProvider::new().unwrap()));
    }

    // Step 3a: Try to load the incremental parse cache. Best-effort —
    // a missing/corrupt/version-mismatched cache silently falls back to
    // a full re-parse. The cache file lives next to graph.bin under
    // the resolved `<index_dir>` so it inherits the same per-branch isolation.
    let cache_path = layout.index_dir.join("incremental_cache.bin");
    let cache_disabled =
        args.no_cache || std::env::var("GNX_NO_CACHE").is_ok_and(|v| !v.is_empty() && v != "0");
    let cache_index = if cache_disabled {
        None
    } else {
        crate::incremental_cache::load_cache(&cache_path)
    };
    let cache_count_pre = cache_index.as_ref().map(|c| c.len()).unwrap_or(0);
    // Tracks the exact number of files that hit the cache (vs the
    // misleading "min(pre, post)" upper bound). `AtomicUsize` because
    // the closure is called concurrently across rayon worker threads.
    let cache_hits_counter = std::sync::atomic::AtomicUsize::new(0);

    // Step 3b: Analyze and load embeddings cache concurrently. The parse
    // cache is consulted per-file inside `analyze_with_cache`; the
    // embeddings cache (separate concept) still pre-loads serially here
    // when `--embeddings` is on.
    let (local_graphs, (old_file_hashes, old_embeddings_cache)) = rayon::join(
        || {
            let cache_ref = cache_index.as_ref();
            let hits = &cache_hits_counter;
            pipeline.analyze_with_cache(files_to_analyze, |rel_path, content_hash| {
                cache_ref
                    .and_then(|c| c.get(rel_path, content_hash))
                    .inspect(|_| {
                        hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    })
            })
        },
        || {
            let mut hashes = std::collections::HashMap::new();
            let mut embs = std::collections::HashMap::new();
            if !args.force {
                if let Ok(old_engine) = crate::engine::Engine::load(&bin_path) {
                    if let Ok(old_graph) = old_engine.graph() {
                        for file in old_graph.files.iter() {
                            let path = file.path.resolve(&old_graph.string_pool);
                            hashes.insert(path.to_string(), file.content_hash);
                        }
                        if embeddings_flag && !args.drop_embeddings {
                            if let rkyv::option::ArchivedOption::Some(old_embs) =
                                &old_graph.embeddings
                            {
                                for (idx, node) in old_graph.nodes.iter().enumerate() {
                                    if let Some(emb) = old_embs.get(idx) {
                                        let uid = node.uid.resolve(&old_graph.string_pool);
                                        let vec_f32: Vec<f32> =
                                            emb.iter().map(|x| x.to_native()).collect();
                                        embs.insert(uid.to_string(), vec_f32);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            (hashes, embs)
        },
    );
    let analyze_duration = analyze_start.elapsed();

    let cache_count_post = local_graphs.len();
    let cache_hits = cache_hits_counter.load(std::sync::atomic::Ordering::Relaxed);

    // Snapshot every `LocalGraph` into a Vec<CachedEntry> *before* the
    // for-loop below consumes `local_graphs`. One unavoidable clone per
    // file — both `builder.add_graph` and `save_cache` need owned
    // `LocalGraph` instances. Skip the snapshot entirely when cache is
    // disabled to avoid the ~per-file clone cost.
    let cache_entries: Option<Vec<crate::incremental_cache::CachedEntry>> = if cache_disabled {
        None
    } else {
        Some(
            local_graphs
                .iter()
                .map(|lg| crate::incremental_cache::CachedEntry {
                    file_path: lg.file_path.clone(),
                    content_hash: lg.content_hash,
                    local_graph: lg.clone(),
                })
                .collect(),
        )
    };

    // Step 4: Build global graph
    let build_start = Instant::now();
    let aliases = crate::config_parser::parse_configs(&repo_path);
    let mut builder = GraphBuilder::new()
        .with_embeddings(args.embeddings)
        .with_cache(old_file_hashes, old_embeddings_cache)
        .with_resolver_dump(args.dump_resolver.clone())
        .with_path_aliases(aliases)
        .with_repo_root(repo_path.clone());
    for graph in local_graphs {
        builder.add_graph(graph);
    }
    let global_graph = builder.build();
    let build_duration = build_start.elapsed();

    // Step 4.5: Persist the incremental cache (best-effort; errors logged
    // but never propagated). Runs after build but before save graph.bin
    // so a cache-write failure can't masquerade as a graph-write failure
    // in user-visible logs.
    if let Some(entries) = cache_entries {
        crate::incremental_cache::save_cache(&cache_path, entries);
    }

    // Step 5: Save graph
    let save_start = Instant::now();

    // Acquire exclusive lock before saving to prevent concurrent write corruption
    let lock_path = bin_path.with_extension("lock");
    let _lock = graph_nexus_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| format!("Failed to acquire index lock: {}", e))?;

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph)
        .map_err(|e| format!("Serialization error: {}", e))?;
    // Atomic write: a Ctrl+C between `write_all` and `rename` leaves a
    // recognizable `graph.bin.tmp` sibling, never a half-written
    // `graph.bin` that the next reader would mmap and segfault on.
    graph_nexus_core::registry::atomic_write_bytes(&bin_path, &bytes)
        .map_err(|e| format!("Failed to write graph.bin: {}", e))?;
    let save_duration = save_start.elapsed();

    // Meta + registry + audit (post-save)
    let node_count = global_graph.nodes.len();
    let file_count = global_graph.files.len();
    let indexed_at = chrono::Utc::now().to_rfc3339();
    let meta = graph_nexus_core::registry::BranchMeta {
        indexed_at: indexed_at.clone(),
        node_count: node_count as u32,
        delta_size: 0,
        last_compact_at: None,
        worktree_path: state.worktree_path.to_string_lossy().into(),
        remote_url: state
            .remote_url
            .as_deref()
            .map(graph_nexus_core::registry::strip_credentials)
            .unwrap_or_default(),
        schema_version: 1,
    };
    graph_nexus_core::registry::BranchMeta::write_atomic(&meta_path, &meta)
        .map_err(|e| format!("Failed to write meta.json: {e}"))?;

    {
        let mut registry = graph_nexus_core::registry::Registry::open(&home_gnx)
            .map_err(|e| format!("registry reopen: {e}"))?;
        let branch_entry = graph_nexus_core::registry::BranchEntry {
            name: state.branch.clone(),
            index_dir: layout.index_dir.to_string_lossy().into(),
            indexed_at: indexed_at.clone(),
            node_count: node_count as u32,
            delta_size: 0,
            embedding_status: if args.embeddings {
                "in_progress".into()
            } else {
                "skipped".into()
            },
        };
        let mut branches = vec![branch_entry.clone()];
        if let Some(existing) = registry
            .snapshot()
            .repos
            .iter()
            .find(|r| r.name == state.repo_name)
        {
            branches = existing.branches.clone();
            if let Some(b) = branches.iter_mut().find(|b| b.name == state.branch) {
                *b = branch_entry;
            } else {
                branches.push(branch_entry);
            }
        }
        let repo_entry = graph_nexus_core::registry::RepoEntry {
            name: state.repo_name.clone(),
            remote_url: meta.remote_url.clone(),
            worktree_path: state.worktree_path.to_string_lossy().into(),
            index_dir_root: home_gnx.join(&state.repo_name).to_string_lossy().into(),
            branches,
            groups: vec![],
        };
        registry
            .upsert_repo(repo_entry)
            .map_err(|e| format!("registry upsert: {e}"))?;
    }

    if let Ok(audit) = graph_nexus_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&graph_nexus_core::registry::AuditEvent::AnalyzeComplete {
            repo: state.repo_name.clone(),
            branch: state.branch.clone(),
            files: file_count as u32,
            nodes: node_count as u32,
            duration_ms: start_time.elapsed().as_millis() as u64,
        });
    }

    // Step 6: Build Tantivy Index (best-effort — graph.bin is the
    // primary artifact; BM25 fallback degrades to exact-name resolution
    // if the writer lock is held by a zombie or the prior commit is
    // corrupt, and self-heals on the next analyze run).
    let index_start = Instant::now();
    if let Err(e) = crate::search::TantivyEngine::build_index(&layout.index_dir, &global_graph) {
        if !args.quiet {
            eprintln!(
                "warning: full-text index build failed ({e}); exact-name queries still work — rerun `gnx analyze` to retry"
            );
        }
    }
    let index_duration = index_start.elapsed();

    let total_duration = start_time.elapsed();

    if skipped_large_files > 0 && !args.quiet {
        eprintln!(
            "Skipped: {} files > 512KB (preventing memory exhaustion).",
            skipped_large_files
        );
    }

    if !args.quiet {
        println!("Graph analysis complete.");
        println!("  Scan time:    {:?}", scan_duration);
        println!("  Analyze time: {:?}", analyze_duration);
        println!("  Build time:   {:?}", build_duration);
        println!("  Save time:    {:?}", save_duration);
        println!("  Index time:   {:?}", index_duration);
        println!("  Total time:   {:?}", total_duration);
        if cache_disabled {
            println!(
                "  Cache:        disabled ({} files re-parsed)",
                cache_count_post
            );
        } else if cache_count_pre == 0 {
            println!(
                "  Cache:        first-run, building cache from {} files",
                cache_count_post
            );
        } else {
            let reparsed = cache_count_post.saturating_sub(cache_hits);
            println!(
                "  Cache:        {} reused / {} re-parsed (cache had {} entries)",
                cache_hits, reparsed, cache_count_pre
            );
        }
        println!("Graph saved to {:?}", bin_path);
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
