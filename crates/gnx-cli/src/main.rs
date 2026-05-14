use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod engine;
mod git;
mod git_state;
mod graph_path;
mod output;
pub mod search;

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
    /// List all API routes
    RouteMap(commands::route_map::RouteMapArgs),
    /// Detect changed symbols & affected execution flows from git diff
    DetectChanges(commands::detect_changes::DetectChangesArgs),
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

    // Determine the repo root to use for registry resolution: prefer --repo arg, fall back to cwd.
    let repo_opt = match &cli.command {
        Commands::Context(args) => args.repo.as_deref(),
        Commands::Query(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::RouteMap(args) => args.repo.as_deref(),
        Commands::DetectChanges(args) => args.repo.as_deref(),
        Commands::Analyze(_) => None,
    };
    let cwd = repo_opt
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let graph_path = graph_path::resolve(&cli.graph, &cwd);

    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
            std::process::exit(1);
        }
    };

    let result: Result<(), gnx_core::GnxError> = match cli.command {
        Commands::Context(args) => commands::context::run(args, &engine),
        Commands::Query(args) => commands::query::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::RouteMap(args) => commands::route_map::run(args, &engine),
        Commands::DetectChanges(args) => commands::detect_changes::run(args, &engine),
        Commands::Analyze(_) => Ok(()), // Handled above
    };

    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}
