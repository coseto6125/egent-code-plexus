use clap::Args;
use gnx_analyzer::resolution::builder::GraphBuilder;
use gnx_analyzer::{
    c::parser::CProvider, c_sharp::parser::CSharpProvider, cpp::parser::CppProvider,
    dart::parser::DartProvider, go::parser::GoProvider, java::parser::JavaProvider,
    javascript::parser::JavaScriptProvider, kotlin::parser::KotlinProvider, php::parser::PhpProvider,
    python::parser::PythonProvider, ruby::parser::RubyProvider, rust::parser::RustProvider,
    swift::parser::SwiftProvider, typescript::parser::TypeScriptProvider,
    markdown::parser::MarkdownProvider, yaml::parser::YamlProvider,
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

    let walker = WalkBuilder::new(&repo_path)
        .hidden(false)
        .build();

    for result in walker {
        match result {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                        match ext {
                            "ts" | "tsx" | "py" | "pyi" | "go" | "rs" | "java" | "js" | "jsx"
                            | "mjs" | "cjs" | "php" | "rb" | "kt" | "kts" | "cs" | "c" | "h"
                            | "cpp" | "hpp" | "cc" | "hh" | "cxx" | "hxx" | "swift" | "dart"
                            | "md" | "txt" | "rst" => {
                                let rel_path = path.strip_prefix(&repo_path).unwrap_or(path);
                                files_to_analyze.push((path.to_path_buf(), rel_path.to_path_buf()));
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(err) => {
                tracing::warn!("Error accessing path during scan: {}", err);
            }
        }
    }
    let scan_duration = scan_start.elapsed();

    let gitnexus_dir = repo_path.join(".gitnexus-rs");
    let bin_path = gitnexus_dir.join("graph.bin");
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
                        if let rkyv::option::ArchivedOption::Some(old_embs) = &old_graph.embeddings {
                            for (idx, node) in old_graph.nodes.iter().enumerate() {
                                if let Some(emb) = old_embs.get(idx) {
                                    let uid = node.uid.resolve(&old_graph.string_pool);
                                    let vec_f32: Vec<f32> = emb.iter().map(|x| x.to_native()).collect();
                                    embs.insert(uid.to_string(), vec_f32);
                                }
                            }
                        }
                    }
                }
            }
            (hashes, embs)
        }
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
    std::fs::create_dir_all(&gitnexus_dir)
        .map_err(|e| format!("Failed to create .gitnexus-rs dir: {}", e))?;

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&global_graph)
        .map_err(|e| format!("Serialization error: {}", e))?;
    let mut file =
        File::create(&bin_path).map_err(|e| format!("Failed to create graph.bin: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Failed to write to graph.bin: {}", e))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync graph.bin: {}", e))?;
    let save_duration = save_start.elapsed();

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
