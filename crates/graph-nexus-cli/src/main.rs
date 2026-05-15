use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

mod admin;
mod auto_ensure;
mod commands;
mod config_parser;
mod engine;
mod git;
mod git_state;
mod graph_path;
mod hint;
mod incremental_cache;
mod output;
pub mod reanalyze;
mod repo_selector;
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
    /// Show symbol's full context: signature, body, edges, callers, overrides, and 1-hop upstream impact
    Inspect(commands::inspect::InspectArgs),
    /// Find symbols by name or concept (auto bm25 / hybrid / vector)
    Search(commands::search::SearchArgs),
    /// Blast radius — from <name> or git diff via --since <ref>
    Impact(commands::impact::ImpactArgs),
    /// AST-aware multi-file rename
    Rename(commands::rename::RenameArgs),
    /// Cypher query escape hatch
    Cypher(commands::cypher::CypherArgs),
    /// Registry + repo health (indexed repos, freshness, frameworks, externals, blind spots)
    Coverage(commands::coverage::CoverageArgs),
    /// List HTTP routes; with path, show handler + caller chain
    Routes(commands::routes::RoutesArgs),
    /// Verify a file's symbol references exist in the graph
    Scan(commands::scan::ScanArgs),
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),

    /// Administrative operations. With no subcommand: launches the interactive
    /// TUI for host-integration management. With a subcommand: runs that
    /// admin operation (registry / hooks / destructive ops — hidden namespace).
    #[command(hide = true)]
    Admin {
        #[command(subcommand)]
        command: Option<commands::admin::AdminCommands>,
    },

    /// Internal: process reference-transaction events (called by git hook)
    #[command(hide = true)]
    HookHandle(commands::hook_handle::HookHandleArgs),
    /// Internal: detached watcher dispatched by hook-handle
    #[command(hide = true)]
    HookWatcher(commands::hook_watcher::HookWatcherArgs),
    /// Internal: diff resolver dump against language oracle (gnx-dev QA)
    #[command(hide = true)]
    VerifyResolver(commands::verify_resolver::VerifyResolverArgs),
    /// Internal: HTTP consumer key vs route response shape check
    /// (will fold into `contracts` in a future task)
    #[command(hide = true)]
    ShapeCheck(commands::shape_check::ShapeCheckArgs),
    /// Internal: MCP transport (serve | tools) — for external agents talking to gnx.
    #[command(hide = true)]
    Mcp(commands::mcp::McpArgs),
}

fn main() {
    // Default to WARN so tantivy / parser INFO chatter doesn't reach agents'
    // stderr. RUST_LOG=info / debug overrides for human debugging.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = Cli::parse();

    // Admin: subcommand → run the admin operation; no subcommand → launch TUI.
    if let Commands::Admin { command } = cli.command {
        let err = match command {
            Some(cmd) => commands::admin::run(cmd),
            None => admin::run(admin::AdminArgs {}),
        };
        if let Err(e) = err {
            eprintln!("Command failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Dispatch table for commands that don't need a graph loaded.
    macro_rules! run_no_graph {
        ($expr:expr) => {{
            if let Err(e) = $expr {
                eprintln!("Command failed: {e}");
                std::process::exit(1);
            }
            return;
        }};
    }

    match &cli.command {
        Commands::HookHandle(args) => run_no_graph!(commands::hook_handle::run(args.clone())),
        Commands::HookWatcher(args) => run_no_graph!(commands::hook_watcher::run(args.clone())),
        Commands::VerifyResolver(args) => {
            run_no_graph!(commands::verify_resolver::run(args.clone()))
        }
        Commands::Coverage(args) => {
            run_no_graph!(commands::coverage::run(args.clone(), &cli.graph))
        }
        Commands::Contracts(args) => run_no_graph!(commands::contracts::run(args.clone())),
        Commands::Mcp(args) => {
            run_no_graph!(commands::mcp::run(args.clone(), Cli::command()))
        }
        _ => {} // fall through to graph-loading path
    }

    // Agent commands + ShapeCheck (hidden internal) — need graph
    let repo_opt = match &cli.command {
        Commands::Inspect(args) => args.repo.as_deref(),
        Commands::Search(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::Rename(args) => args.repo.as_deref(),
        Commands::Cypher(args) => args.repo.as_deref(),
        Commands::Routes(args) => args.repo.as_deref(),
        Commands::Scan(args) => args.repo.as_deref(),
        Commands::ShapeCheck(args) => args.repo.as_deref(),
        Commands::Coverage(_)
        | Commands::Contracts(_)
        | Commands::Admin { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::VerifyResolver(_)
        | Commands::Mcp(_) => None,
    };
    let cwd = repo_opt
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let graph_path = graph_path::resolve(&cli.graph, &cwd);

    if let Err(err) = auto_ensure::ensure_fresh(&graph_path, &cwd) {
        eprintln!("Error preparing index for {}: {err}", cwd.display());
        std::process::exit(1);
    }

    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
            std::process::exit(1);
        }
    };

    let result: Result<(), graph_nexus_core::GnxError> = match cli.command {
        Commands::Inspect(args) => commands::inspect::run(args, &engine, &graph_path),
        Commands::Search(args) => commands::search::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::Rename(args) => commands::rename::run(args, &engine),
        Commands::Cypher(args) => commands::cypher::run(args, &engine),
        Commands::Routes(args) => commands::routes::run(args, &engine),
        Commands::Scan(args) => commands::scan::run(args, &engine),
        Commands::ShapeCheck(args) => commands::shape_check::run(args, &engine),
        Commands::Coverage(_)
        | Commands::Contracts(_)
        | Commands::Admin { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::VerifyResolver(_)
        | Commands::Mcp(_) => {
            unreachable!("handled before graph load")
        }
    };
    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}
