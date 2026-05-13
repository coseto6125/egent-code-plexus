use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod engine;

use engine::Engine;

#[derive(Parser)]
#[command(name = "gnx-rs", version = "0.1.0", about = "GitNexus stateless query engine (mmap)")]
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
}

fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let engine = match Engine::load(&cli.graph) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", cli.graph.display(), err);
            std::process::exit(1);
        }
    };

    let result = match cli.command {
        Commands::Context(args) => commands::context::run(args, &engine),
        Commands::Query(args) => commands::query::run(args, &engine),
    };

    if let Err(e) = result {
        eprintln!("Command failed: {}", e);
        std::process::exit(1);
    }
}