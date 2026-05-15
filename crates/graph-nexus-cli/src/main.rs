use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod config_parser;
mod engine;
mod git;
mod git_state;
mod graph_path;
mod incremental_cache;
mod output;
pub mod reanalyze;
pub mod search;

use engine::Engine;

#[derive(Parser)]
#[command(
    name = "graph-nexus",
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
    #[command(alias = "route_map")]
    RouteMap(commands::route_map::RouteMapArgs),
    /// Detect changed symbols & affected execution flows from git diff
    #[command(alias = "detect_changes")]
    DetectChanges(commands::detect_changes::DetectChangesArgs),
    /// Install reference-transaction hook for branch tracking
    Init(commands::init::InitArgs),
    /// Internal: process reference-transaction events (called by git hook).
    #[command(hide = true)]
    HookHandle(commands::hook_handle::HookHandleArgs),
    /// Internal: detached watcher dispatched by hook-handle (do not invoke directly).
    #[command(hide = true)]
    HookWatcher(commands::hook_watcher::HookWatcherArgs),
    /// Remove orphan index dir + registry branch entry for the given branch
    Prune(commands::prune::PruneArgs),
    /// Rename a branch's index dir + registry entry
    #[command(alias = "rename_branch")]
    RenameBranch(commands::rename_branch::RenameBranchArgs),
    /// AST-powered multi-file symbol rename across 14 languages
    Rename(commands::rename::RenameArgs),
    /// List all repos in the registry (compact | json | toon)
    List(commands::list::ListArgs),
    /// LLM-friendly project overview (markdown / json) from graph.bin
    Summarize(commands::summarize::SummarizeArgs),
    /// Diff a resolver dump against a language oracle (TS / Python / Rust)
    #[command(alias = "verify_resolver")]
    VerifyResolver(commands::verify_resolver::VerifyResolverArgs),
    /// Print framework coverage + blind-spot catalog + graph status (LLM contract)
    Doctor(commands::doctor::DoctorArgs),
    /// Execute a Cypher query against the graph
    Cypher(commands::cypher::CypherArgs),
    /// Delete a repo's index
    Clean(commands::clean::CleanArgs),
    /// List members of a specific community (cluster)
    Cluster(commands::cluster::ClusterArgs),
    /// Output the per-process step trace
    Process(commands::process::ProcessArgs),
    /// Check per-repo staleness
    Status(commands::status::StatusArgs),
    /// Interactive wizard to edit `.gitnexus-rs/config.toml`
    Config(commands::config::ConfigArgs),
    /// Index the current working directory (wraps `analyze --repo .`)
    #[command(alias = "analyze_here")]
    AnalyzeHere(commands::analyze_here::AnalyzeHereArgs),
    /// Given an HTTP route path, surface handler + upstream callers
    #[command(alias = "api_impact")]
    ApiImpact(commands::api_impact::ApiImpactArgs),
    /// Recovery: register an existing `.gitnexus-rs/` folder into the registry
    Index(commands::index::IndexArgs),
    /// Delete a registry entry by name or absolute path
    Remove(commands::remove::RemoveArgs),
    /// Enumerate calls to known HTTP / DB / Redis / queue clients
    #[command(alias = "tool_map")]
    ToolMap(commands::tool_map::ToolMapArgs),
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

    // AnalyzeHere wraps Analyze with cwd; same no-graph-needed property.
    if let Commands::AnalyzeHere(args) = &cli.command {
        if let Err(e) = commands::analyze_here::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Index is a pure registry write (registers existing `.gitnexus-rs/` into registry).
    if let Commands::Index(args) = &cli.command {
        if let Err(e) = commands::index::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Remove deletes a registry entry + on-disk index folder; no graph needed.
    if let Commands::Remove(args) = &cli.command {
        if let Err(e) = commands::remove::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Init doesn't need a graph either — it installs the git hook
    if let Commands::Init(args) = &cli.command {
        if let Err(e) = commands::init::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // HookHandle reads stdin and spawns watchers; no graph needed
    if let Commands::HookHandle(args) = &cli.command {
        if let Err(e) = commands::hook_handle::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // HookWatcher is a detached child; no graph needed
    if let Commands::HookWatcher(args) = &cli.command {
        if let Err(e) = commands::hook_watcher::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Prune removes orphan index dir + registry entry; no graph needed
    if let Commands::Prune(args) = &cli.command {
        if let Err(e) = commands::prune::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // RenameBranch renames index dir + registry entry; no graph needed
    if let Commands::RenameBranch(args) = &cli.command {
        if let Err(e) = commands::rename_branch::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // List enumerates the registry; no graph needed
    if let Commands::List(args) = &cli.command {
        if let Err(e) = commands::list::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // VerifyResolver diffs JSONL files; no graph needed
    if let Commands::VerifyResolver(args) = &cli.command {
        if let Err(e) = commands::verify_resolver::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Doctor reports a static support contract + graph file status; no graph load needed.
    if let Commands::Doctor(args) = &cli.command {
        if let Err(e) = commands::doctor::run(args.clone(), &cli.graph) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Clean removes the index dir; no graph needed
    if let Commands::Clean(args) = &cli.command {
        if let Err(e) = commands::clean::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Status does basic staleness checks; no heavy graph loaded needed
    if let Commands::Status(args) = &cli.command {
        if let Err(e) = commands::status::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Config TUI is fully self-contained — reads / writes TOML only.
    if let Commands::Config(args) = &cli.command {
        if let Err(e) = commands::config::run(args.clone()) {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Determine the repo root to use for registry resolution: prefer --repo arg, fall back to cwd.
    let repo_opt = match &cli.command {
        Commands::Context(args) => args.repo.as_deref(),
        Commands::Query(args) => args.repo.as_deref(),
        Commands::Cypher(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::RouteMap(args) => args.repo.as_deref(),
        Commands::DetectChanges(args) => args.repo.as_deref(),
        Commands::Summarize(args) => args.repo.as_deref(),
        Commands::Rename(args) => args.repo.as_deref(),
        Commands::Process(args) => args.repo.as_deref(),
        Commands::Cluster(args) => args.repo.as_deref(),
        Commands::Status(args) => args.repo.as_deref(),
        Commands::Clean(args) => args.repo.to_str(),
        Commands::Config(args) => args.repo.as_deref(),
        Commands::ApiImpact(args) => args.repo.as_deref(),
        Commands::ToolMap(args) => args.repo.as_deref(),
        Commands::Analyze(_)
        | Commands::AnalyzeHere(_)
        | Commands::Init(_)
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Prune(_)
        | Commands::RenameBranch(_)
        | Commands::List(_)
        | Commands::VerifyResolver(_)
        | Commands::Doctor(_)
        | Commands::Index(_)
        | Commands::Remove(_) => None,
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

    let result: Result<(), graph_nexus_core::GnxError> = match cli.command {
        Commands::Context(args) => commands::context::run(args, &engine),
        Commands::Query(args) => commands::query::run(args, &engine),
        Commands::Cypher(args) => commands::cypher::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::RouteMap(args) => commands::route_map::run(args, &engine),
        Commands::DetectChanges(args) => commands::detect_changes::run(args, &engine),
        Commands::Summarize(args) => commands::summarize::run(args, &engine),
        Commands::Rename(args) => commands::rename::run(args, &engine),
        Commands::Process(args) => commands::process::run(args, &engine),
        Commands::Cluster(args) => commands::cluster::run(args, &engine),
        Commands::ApiImpact(args) => commands::api_impact::run(args, &engine),
        Commands::ToolMap(args) => commands::tool_map::run(args, &engine),
        Commands::Analyze(_)
        | Commands::AnalyzeHere(_)
        | Commands::Init(_)
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Prune(_)
        | Commands::RenameBranch(_)
        | Commands::List(_)
        | Commands::VerifyResolver(_)
        | Commands::Doctor(_)
        | Commands::Clean(_)
        | Commands::Status(_)
        | Commands::Config(_)
        | Commands::Index(_)
        | Commands::Remove(_) => Ok(()), // Handled above
    };

    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}
