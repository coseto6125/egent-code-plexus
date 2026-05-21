use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

// mimalloc as global allocator: heavily parallel build path
// (16-thread rayon par_iter on 22k file parses + cache puts + edge
// emission) hammers the system allocator. mimalloc's per-thread
// arenas dramatically reduce allocator lock contention vs glibc
// malloc, especially for the many short-lived Vec/String allocations
// in tree-sitter capture processing + post-process edge resolution.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod admin;
mod auto_ensure;
mod background;
mod build;
mod commands;
mod commit_lookup;
mod config_parser;
mod engine;
mod git;
mod git_state;
mod graph_path;
mod hint;
mod output;
mod parse_cache;
mod peer;
pub mod reanalyze;
mod repo_identity;
mod repo_selector;
pub mod search;
mod session;

pub(crate) const ECP_IGNORE_FILE: &str = ".ecpignore";

use engine::Engine;

#[derive(Parser)]
#[command(
    name = "ecp",
    version = env!("CARGO_PKG_VERSION"),
    about = "egent-code-plexus stateless query engine (mmap)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to the graph.bin file
    #[arg(long, default_value = ".ecp/graph.bin", global = true)]
    graph: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// Show symbol's full context: signature, body, edges, callers, overrides, and 1-hop upstream impact
    Inspect(commands::inspect::InspectArgs),
    /// Locate symbols by exact name (default), substring (`--mode fuzzy`), or BM25 lexical ranking (`--mode bm25`). Exact / fuzzy return a single most-likely definition (or all via `--all`); bm25 returns top-K partitioned into source / tests / reference / document / config buckets and supports stdin `--batch`.
    Find(commands::find::FindArgs),
    /// Symbol blast radius — affected callers + risk_level. For binding tier-degradation or resolver delta, use `ecp diff`.
    Impact(commands::impact::ImpactArgs),
    /// AST-aware multi-file rename
    Rename(commands::rename::RenameArgs),
    /// Cypher query escape hatch
    Cypher(commands::cypher::CypherArgs),
    /// Registry + repo health (indexed repos, freshness, frameworks, blind spots).
    /// External-client (HTTP/DB/Redis/queue) usage detail: see `ecp tool-map`.
    Coverage(commands::coverage::CoverageArgs),
    /// List HTTP routes; with path, show handler + caller chain
    Routes(commands::routes::RoutesArgs),
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),
    /// Edge-level resolver delta — binding tier-degradation (silent break), route / contract changes. For symbol blast-radius, use `ecp impact`.
    Diff(commands::diff::DiffArgs),

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
    /// Detect drift between HTTP consumer access patterns and Route response shapes.
    ShapeCheck(commands::shape_check::ShapeCheckArgs),
    /// Enumerate calls to external HTTP/DB/Redis/queue clients via per-file import-binding analysis.
    ToolMap(commands::tool_map::ToolMapArgs),
    /// Internal: Claude Code / Codex / Gemini agent hook dispatch.
    #[command(hide = true)]
    Hook(commands::hook::HookArgs),
    /// Relay this session's dirty surface to peer inboxes (foreground / detached daemon).
    /// MCP-hidden: lifecycle is owned by the session_start hook, not the LLM.
    #[command(hide = true)]
    Watch(commands::watch::WatchArgs),
    /// Multi-session peer collaboration (status / diff / log / gc + Ƀ messaging)
    Peers(commands::peers::PeersArgs),
    /// LLM-workflow audit aggregator — runs impact, coverage (blind-spot),
    /// egress (tool-map), shape-check, and resolver-diff over changed files in
    /// one shot, filtered to high-confidence signals only.
    Review(commands::review::ReviewArgs),
    /// Multi-repo group contract extraction and cross-link matching
    #[command(hide = true)]
    Group {
        #[command(subcommand)]
        cmd: commands::group::GroupCommands,
    },
    /// Heuristic Saga compensate/undo/rollback name-pair detector.
    /// All findings carry `requires_verification: true`; never enters the graph.
    FindTransactionPatterns(commands::find_tx_patterns::FindTxPatternsArgs),
    /// List `EventTopicMirror` heuristic edges: (publisher_fn, subscriber_fn, topic, confidence).
    /// Edges are emitted by T5-33 at confidence=0.85; filter with --min-confidence, --topic, --lib.
    FindEventMirrors(commands::find_event_mirrors::FindEventMirrorsArgs),
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

    maybe_spawn_background_gc();

    let cli = Cli::parse();

    // Gatekeeper: any top-level command with `--repo @<group>` exits early
    // with a migration hint pointing at `ecp group …`. Runs before all
    // dispatch so the message is identical regardless of graph-free vs
    // graph-loading path. `@all` is unaffected (still resolves to the
    // registered repo set).
    check_group_atom(&cli);

    // Admin: subcommand → run the admin operation; no subcommand → launch TUI.
    if let Commands::Admin { command } = cli.command {
        let err = match command {
            Some(cmd) => commands::admin::run(cmd, Cli::command()),
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
        Commands::Coverage(args) => {
            run_no_graph!(commands::coverage::run(args.clone(), &cli.graph))
        }
        Commands::Contracts(args) => run_no_graph!(commands::contracts::run(args.clone())),
        Commands::Diff(args) => run_no_graph!(commands::diff::run(args.clone())),
        Commands::Hook(args) => run_no_graph!(commands::hook::run(args.clone())),
        Commands::Watch(args) => run_no_graph!(commands::watch::run(args.clone())),
        Commands::Peers(args) => run_no_graph!(commands::peers::run(args.clone())),
        Commands::Group { cmd } => run_no_graph!(commands::group::run(cmd.clone())),
        _ => {} // fall through to graph-loading path
    }

    // Agent commands + ShapeCheck (hidden internal) — need graph
    let repo_opt = match &cli.command {
        Commands::Inspect(args) => args.repo.as_deref(),
        Commands::Find(args) => args.repo.as_deref(),
        Commands::Impact(args) => args.repo.as_deref(),
        Commands::Rename(args) => args.repo.as_deref(),
        Commands::Cypher(args) => args.repo.as_deref(),
        Commands::Routes(args) => args.repo.as_deref(),
        Commands::ShapeCheck(args) => args.repo.as_deref(),
        Commands::ToolMap(args) => args.repo.as_deref(),
        Commands::Review(args) => args.repo.as_deref(),
        Commands::FindTransactionPatterns(args) => args.repo.as_deref(),
        Commands::FindEventMirrors(_) => None,
        Commands::Coverage(_)
        | Commands::Contracts(_)
        | Commands::Diff(_)
        | Commands::Admin { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Hook(_)
        | Commands::Watch(_)
        | Commands::Peers(_)
        | Commands::Group { .. } => None,
    };
    let cwd = repo_opt
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut graph_path = graph_path::resolve(&cli.graph, &cwd);

    if let Err(err) = auto_ensure::ensure_fresh(&graph_path, &cwd) {
        eprintln!("Error preparing index for {}: {err}", cwd.display());
        std::process::exit(1);
    }
    graph_path = graph_path::resolve(&cli.graph, &cwd);

    let engine = match Engine::load(&graph_path) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Error loading graph from {}: {}", graph_path.display(), err);
            std::process::exit(1);
        }
    };

    let result: Result<(), ecp_core::EcpError> = match cli.command {
        Commands::Inspect(args) => commands::inspect::run(args, &engine, &graph_path),
        Commands::Find(args) => commands::find::run(args, &engine),
        Commands::Impact(args) => commands::impact::run(args, &engine),
        Commands::Rename(args) => commands::rename::run(args, &engine),
        Commands::Cypher(args) => commands::cypher::run(args, &engine),
        Commands::Routes(args) => commands::routes::run(args, &engine),
        Commands::ShapeCheck(args) => commands::shape_check::run(args, &engine),
        Commands::ToolMap(args) => commands::tool_map::run(args, &engine),
        Commands::Review(args) => commands::review::run(args, &engine),
        Commands::FindTransactionPatterns(args) => commands::find_tx_patterns::run(args, &engine),
        Commands::FindEventMirrors(args) => commands::find_event_mirrors::run(args, &engine),
        Commands::Coverage(_)
        | Commands::Contracts(_)
        | Commands::Diff(_)
        | Commands::Admin { .. }
        | Commands::HookHandle(_)
        | Commands::HookWatcher(_)
        | Commands::Hook(_)
        | Commands::Watch(_)
        | Commands::Peers(_)
        | Commands::Group { .. } => unreachable!("handled before graph load"),
    };
    if let Err(e) = result {
        eprintln!("Command failed: {e}");
        std::process::exit(1);
    }
}

/// Top-level `--repo @<group>` rejection. The atom is meaningful only inside
/// `ecp group <verb>`; on every other command it is either a path-not-found
/// (auto_ensure) or a single-repo selector that silently expands and fails
/// later with an opaque message. Catch it here and exit with a clear hint.
///
/// The `hint` is the `ecp group <verb>` migration target. Commands without a
/// group analog (inspect / rename / cypher / routes / shape-check / tool-map
/// / review / diff) carry `None` and get redirected to `ecp group --help`.
fn check_group_atom(cli: &Cli) {
    // The `repo: Option<String>` accessor lives on each variant's args struct,
    // so the match has to enumerate them. Pull the value out first; bail
    // fast for commands without a `--repo` field (contracts/coverage are
    // already protected via resolve_top_level; peers/admin/hooks don't expose
    // a group-aware selector).
    let (repo_opt, hint): (Option<&str>, Option<&str>) = match &cli.command {
        Commands::Find(a) => (a.repo.as_deref(), Some("find")),
        Commands::Impact(a) => (a.repo.as_deref(), Some("impact")),
        Commands::Inspect(a) => (a.repo.as_deref(), None),
        Commands::Rename(a) => (a.repo.as_deref(), None),
        Commands::Cypher(a) => (a.repo.as_deref(), None),
        Commands::Routes(a) => (a.repo.as_deref(), None),
        Commands::ShapeCheck(a) => (a.repo.as_deref(), None),
        Commands::ToolMap(a) => (a.repo.as_deref(), None),
        Commands::Review(a) => (a.repo.as_deref(), None),
        Commands::Diff(a) => (a.repo.as_deref(), None),
        Commands::FindTransactionPatterns(a) => (a.repo.as_deref(), None),
        Commands::FindEventMirrors(_) => return,
        _ => return,
    };
    // The vast majority of invocations don't pass `--repo` at all, so the
    // two early returns below fire before any further work.
    let Some(sel) = repo_opt else { return };
    let Some(group_name) = sel.strip_prefix('@') else {
        return;
    };
    if group_name == "all" {
        return;
    }
    match hint {
        Some(verb) => eprintln!(
            "error: `@{group_name}` cannot be used at the top level — use `ecp group {verb}` instead"
        ),
        None => eprintln!(
            "error: `@{group_name}` cannot be used at the top level — this command is single-repo; see `ecp group --help` for cross-repo workflows"
        ),
    }
    std::process::exit(1);
}

/// Auto-trigger background GC when the home heartbeat stamp is missing
/// or older than 24h. Spawned detached; failures are silent (best-effort).
fn maybe_spawn_background_gc() {
    let home = ecp_core::registry::resolve_home_ecp();
    let stamp = home.join(".last-gc");
    let stale = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() > 24 * 3600)
        .unwrap_or(true);
    if !stale {
        return;
    }
    // Touch the stamp synchronously so concurrent CLI invocations don't all spawn.
    let _ = std::fs::create_dir_all(&home);
    let _ = std::fs::write(&stamp, b"");
    // Detach background sweep — gc admin command not yet wired (Phase 8 Task 8.5),
    // so until then this fn just touches the stamp. Once `ecp admin gc` lands,
    // change the body to spawn it as a detached subprocess.
}
