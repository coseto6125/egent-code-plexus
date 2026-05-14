use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod engine;

use engine::Engine;

#[derive(Parser)]
#[command(
    name = "gnx-rs",
    version = "0.1.0",
    about = "GitNexus stateless query engine (mmap)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to the graph.bin file
    #[arg(long, default_value = ".gitnexus-rs/graph.bin", global = true)]
    graph: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// Context query command matching GitNexus
    Context(commands::context::ContextArgs),
    /// Search for symbols by name
    Query(commands::query::QueryArgs),
    /// Impact blast radius traversal
    Impact(commands::impact::ImpactArgs),
    /// Analyze repository (Mock for parity harness)
    Analyze(commands::analyze::AnalyzeArgs),
}

fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Analyze command doesn't need to load the graph first, it creates it
    if let Commands::Analyze(args) = &cli.command {
        if let Err(e) = commands::analyze::run(args.clone()) {
            // needs Clone for args or pass by ref, maybe refactoring needed, wait... actually just pass by value
            eprintln!("Command failed: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let mut graph_path = cli.graph;

    // Attempt to extract repo from subcommand args if available to override default graph path
    let repo_opt = match &cli.command {
        Commands::Context(args) => args.repo.as_ref(),
        Commands::Query(args) => args.repo.as_ref(),
        Commands::Impact(args) => args.repo.as_ref(),
        Commands::Analyze(_) => None,
    };

    if let Some(repo) = repo_opt {
        graph_path = std::path::PathBuf::from(repo).join(".gitnexus-rs/graph.bin");
    }

    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
            std::process::exit(1);
        }
    };

    let result = match cli.command {
        Commands::Context(args) => commands::context::run(args, &engine),
        Commands::Query(args) => commands::query::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::Analyze(_) => Ok(()), // Handled above
    };

    if let Err(e) = result {
        eprintln!("Command failed: {}", e);
        std::process::exit(1);
    }
}
