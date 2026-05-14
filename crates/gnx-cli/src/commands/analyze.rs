use clap::Args;
use gnx_analyzer::resolution::builder::GraphBuilder;
use gnx_analyzer::{
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
use gnx_core::analyzer::pipeline::AnalyzerPipeline;
use ignore::WalkBuilder;
use std::fs::File;
use std::io::Write;
use std::time::Instant;

#[derive(Args, Debug, Clone)]
pub struct AnalyzeArgs {
    #[arg(long)]
    pub repo: String,

    #[arg(long, default_value_t = false)]
    pub embeddings: bool,
}

pub fn run(args: AnalyzeArgs) -> Result<(), String> {
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

    let walker = WalkBuilder::new(&repo_path).hidden(false).build();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
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

    let home_gnx = gnx_core::registry::resolve_home_gnx();

    let existing_repos: Vec<(String, String)> = {
        let reg = gnx_core::registry::Registry::open(&home_gnx)
            .map_err(|e| format!("registry open: {e}"))?;
        reg.snapshot()
            .repos
            .iter()
            .map(|r| (r.name.clone(), r.worktree_path.clone()))
            .collect()
    };
    let layout = gnx_core::registry::IndexLayout::resolve(
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

    // Step 2: Initialize pipeline and register providers
    let analyze_start = Instant::now();
    let mut pipeline = AnalyzerPipeline::new();
    pipeline.register_provider(Box::new(TypeScriptProvider::new().unwrap()));
    pipeline.register_provider(Box::new(PythonProvider::new().unwrap()));
    pipeline.register_provider(Box::new(GoProvider::new().unwrap()));
    pipeline.register_provider(Box::new(RustProvider::new().unwrap()));
    pipeline.register_provider(Box::new(JavaProvider::new().unwrap()));
    pipeline.register_provider(Box::new(JavaScriptProvider::new().unwrap()));
    pipeline.register_provider(Box::new(PhpProvider::new().unwrap()));
    pipeline.register_provider(Box::new(RubyProvider::new().unwrap()));
    pipeline.register_provider(Box::new(KotlinProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CSharpProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CppProvider::new().unwrap()));
    pipeline.register_provider(Box::new(DartProvider::new().unwrap()));
    pipeline.register_provider(Box::new(MarkdownProvider::new().unwrap()));
    pipeline.register_provider(Box::new(YamlProvider::new().unwrap()));
    pipeline.register_provider(Box::new(GitHubActionsProvider::new().unwrap()));
    pipeline.register_provider(Box::new(BashProvider::new().unwrap()));
    pipeline.register_provider(Box::new(LuaProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CrystalProvider::new().unwrap()));
    pipeline.register_provider(Box::new(MoveProvider::new().unwrap()));
    pipeline.register_provider(Box::new(SolidityProvider::new().unwrap()));
    pipeline.register_provider(Box::new(DockerfileProvider::new().unwrap()));
    pipeline.register_provider(Box::new(NimProvider::new().unwrap()));
    pipeline.register_provider(Box::new(HclProvider::new().unwrap()));
    pipeline.register_provider(Box::new(SqlProvider::new().unwrap()));
    pipeline.register_provider(Box::new(VyperProvider::new().unwrap()));
    pipeline.register_provider(Box::new(VerilogProvider::new().unwrap()));
    pipeline.register_provider(Box::new(CairoProvider::new().unwrap()));
    pipeline.register_provider(Box::new(ZigProvider::new().unwrap()));
    pipeline.register_provider(Box::new(DockerComposeProvider::new().unwrap()));

    // Step 3: Analyze and load cache concurrently
    let (local_graphs, (old_file_hashes, old_embeddings_cache)) = rayon::join(
        || pipeline.analyze(files_to_analyze),
        || {
            let mut hashes = std::collections::HashMap::new();
            let mut embs = std::collections::HashMap::new();
            if embeddings_flag {
                if let Ok(old_engine) = crate::engine::Engine::load(&bin_path) {
                    if let Ok(old_graph) = old_engine.graph() {
                        for file in old_graph.files.iter() {
                            let path = file.path.resolve(&old_graph.string_pool);
                            hashes.insert(path.to_string(), file.content_hash);
                        }
                        if let rkyv::option::ArchivedOption::Some(old_embs) = &old_graph.embeddings
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
            (hashes, embs)
        },
    );
    let analyze_duration = analyze_start.elapsed();

    // Step 4: Build global graph
    let build_start = Instant::now();
    let mut builder = GraphBuilder::new()
        .with_embeddings(args.embeddings)
        .with_cache(old_file_hashes, old_embeddings_cache);
    for graph in local_graphs {
        builder.add_graph(graph);
    }
    let global_graph = builder.build();
    let build_duration = build_start.elapsed();

    // Step 5: Save graph
    let save_start = Instant::now();

    // Acquire exclusive lock before saving to prevent concurrent write corruption
    let lock_path = bin_path.with_extension("lock");
    let _lock = gnx_core::registry::FileLock::acquire_exclusive(&lock_path)
        .map_err(|e| format!("Failed to acquire index lock: {}", e))?;

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph)
        .map_err(|e| format!("Serialization error: {}", e))?;
    let mut file =
        File::create(&bin_path).map_err(|e| format!("Failed to create graph.bin: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Failed to write to graph.bin: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync graph.bin: {}", e))?;
    let save_duration = save_start.elapsed();

    // Meta + registry + audit (post-save)
    let node_count = global_graph.nodes.len();
    let file_count = global_graph.files.len();
    let indexed_at = chrono::Utc::now().to_rfc3339();
    let meta = gnx_core::registry::BranchMeta {
        indexed_at: indexed_at.clone(),
        node_count: node_count as u32,
        delta_size: 0,
        last_compact_at: None,
        worktree_path: state.worktree_path.to_string_lossy().into(),
        remote_url: state
            .remote_url
            .as_deref()
            .map(gnx_core::registry::strip_credentials)
            .unwrap_or_default(),
        schema_version: 1,
    };
    gnx_core::registry::BranchMeta::write_atomic(&meta_path, &meta)
        .map_err(|e| format!("Failed to write meta.json: {e}"))?;

    {
        let mut registry = gnx_core::registry::Registry::open(&home_gnx)
            .map_err(|e| format!("registry reopen: {e}"))?;
        let branch_entry = gnx_core::registry::BranchEntry {
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
        let repo_entry = gnx_core::registry::RepoEntry {
            name: state.repo_name.clone(),
            remote_url: meta.remote_url.clone(),
            worktree_path: state.worktree_path.to_string_lossy().into(),
            index_dir_root: home_gnx.join(&state.repo_name).to_string_lossy().into(),
            branches,
            group: None,
        };
        registry
            .upsert_repo(repo_entry)
            .map_err(|e| format!("registry upsert: {e}"))?;
    }

    if let Ok(audit) = gnx_core::registry::AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&gnx_core::registry::AuditEvent::AnalyzeComplete {
            repo: state.repo_name.clone(),
            branch: state.branch.clone(),
            files: file_count as u32,
            nodes: node_count as u32,
            duration_ms: start_time.elapsed().as_millis() as u64,
        });
    }

    // Step 6: Build Tantivy Index
    let index_start = Instant::now();
    crate::search::TantivyEngine::build_index(&repo_path, &global_graph);
    let index_duration = index_start.elapsed();

    let total_duration = start_time.elapsed();

    println!("Graph analysis complete.");
    println!("  Scan time:    {:?}", scan_duration);
    println!("  Analyze time: {:?}", analyze_duration);
    println!("  Build time:   {:?}", build_duration);
    println!("  Save time:    {:?}", save_duration);
    println!("  Index time:   {:?}", index_duration);
    println!("  Total time:   {:?}", total_duration);
    println!("Graph saved to {:?}", bin_path);

    Ok(())
}
