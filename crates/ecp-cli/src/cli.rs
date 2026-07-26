use crate::commands;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ecp",
    version = concat!(env!("CARGO_PKG_VERSION"), "+", env!("ECP_GIT_SHA")),
    about = "egent-code-plexus stateless query engine (mmap)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to the graph.bin file
    #[arg(long, default_value = ".ecp/graph.bin", global = true)]
    pub graph: PathBuf,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Show a symbol's full context: signature, body, edges, callers, overrides, 1-hop impact
    Inspect(commands::inspect::InspectArgs),
    /// Locate symbols by exact name (default), substring (`--mode fuzzy`), or BM25 ranking (`--mode bm25`).
    ///
    /// Exact / fuzzy return a single most-likely definition (or all via `--all`); bm25 returns top-K
    /// partitioned into source / tests / reference / document / config buckets and supports stdin `--batch`.
    Find(commands::find::FindArgs),
    /// Symbol blast radius — affected callers (counts + call sites).
    ///
    /// For binding tier-degradation or resolver delta, use `ecp diff`.
    Impact(commands::impact::ImpactArgs),
    /// AST-aware multi-file rename
    Rename(commands::rename::RenameArgs),
    /// Cypher query escape hatch
    Cypher(commands::cypher::CypherArgs),
    /// Registry + repo health. Default: scoped to the cwd repo when indexed; registry overview otherwise.
    /// Use `--repo @all` for the registry overview explicitly.
    ///
    /// `blind_spots` lists only LLM-actionable opacity (dynamic-import / reflection / eval);
    /// parser-metric buckets (uid-collision / overload / ifdef-redef) live under `ecp dev uid-audit`.
    /// External-client (HTTP/DB/Redis/queue) usage detail: see `ecp tool-map`.
    Summary(commands::summary::SummaryArgs),
    /// List HTTP routes; with path, show handler + caller chain
    Routes(commands::routes::RoutesArgs),
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),
    /// Edge-level resolver delta — binding tier-degradation (silent break), route / contract changes.
    ///
    /// For symbol blast-radius, use `ecp impact`.
    Diff(commands::diff::DiffArgs),

    /// Remove all ecp host integrations and optionally wipe the index cache.
    ///
    /// Reverses hooks, MCP registration, and skills for Claude Code, Codex,
    /// and Gemini. Use --host to limit to one host; --dry-run to preview.
    Uninstall(commands::uninstall::UninstallArgs),

    /// Administrative operations. With no subcommand: launches the interactive
    /// TUI for host-integration management. With a subcommand: runs that
    /// admin operation (registry / hooks / destructive ops — hidden namespace).
    #[command(hide = true)]
    Admin {
        #[command(subcommand)]
        command: Option<commands::admin::AdminCommands>,
    },

    /// Internal parser-developer audits (uid-collision clusters,
    /// resolver-oracle diffs). Hidden — NOT an LLM-facing surface.
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        command: commands::dev::DevCommands,
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
    /// LLM-workflow audit aggregator over changed files, high-confidence signals only.
    ///
    /// Runs impact, summary (blind-spot), egress (tool-map), shape-check, and
    /// resolver-diff in one shot.
    Review(commands::review::ReviewArgs),
    /// Multi-repo group contract extraction and cross-link matching
    #[command(hide = true)]
    Group {
        #[command(subcommand)]
        cmd: commands::group::GroupCommands,
    },
    /// Heuristic detectors: `saga` (Saga/Outbox), `schema-bindings` (MirrorsField), `event-mirrors` (EventTopicMirror).
    ///
    /// All findings carry `requires_verification: true` and are confidence-tagged; none enter the graph.
    /// Confidence semantics vary by kind — see `ecp heuristics <kind> --help`.
    Heuristics(commands::heuristics::HeuristicsArgs),
    /// Deprecated: use `ecp heuristics saga`
    #[command(hide = true, long_about = "Deprecated: use `ecp heuristics saga`")]
    FindTransactionPatterns(commands::find_tx_patterns::FindTxPatternsArgs),
    /// Deprecated: use `ecp heuristics schema-bindings`
    #[command(
        hide = true,
        long_about = "Deprecated: use `ecp heuristics schema-bindings`"
    )]
    FindSchemaBindings(commands::find_schema_bindings::FindSchemaBindingsArgs),
    /// Deprecated: use `ecp heuristics event-mirrors`
    #[command(
        hide = true,
        long_about = "Deprecated: use `ecp heuristics event-mirrors`"
    )]
    FindEventMirrors(commands::find_event_mirrors::FindEventMirrorsArgs),
    /// Per-language BlindSpot emitter inventory (`schema blindspots`) —
    /// distinguishes "no blind spot in this diff" from "ecp doesn't detect
    /// this dispatch pattern yet" so LLM-context builders can flag gaps.
    /// Hidden because clap's nested-subcommand surface can't be flattened
    /// into a single MCP tool — the matching `ecp_schema` tool is
    /// hand-rolled in `crates/ecp-mcp/src/schema_mcp.rs` with a `subcmd`
    /// discriminator. CLI users keep full `--help` access via
    /// `ecp schema --help` (hidden subcommands still respond to help).
    #[command(hide = true)]
    Schema(commands::schema::SchemaArgs),
    /// List detected Process (execution-flow) nodes, or `processes trace <pattern>` for step sequence.
    ///
    /// Surfaces the Leiden-community + BFS detection emitted at index time
    /// (`pass4_processes` in builder.rs).
    Processes(commands::processes::ProcessesArgs),
    /// Deprecated: use `ecp usage --source mcp`
    #[command(hide = true, long_about = "Deprecated: use `ecp usage --source mcp`")]
    Insight(commands::insight::InsightArgs),
    /// Usage dashboard over CLI + MCP telemetry — counts, p50/p99 latency, error rate.
    ///
    /// Reads `~/.ecp/telemetry/<repo>/{cli-calls,calls}.jsonl`. Default output is
    /// a terminal ASCII dashboard; `--format json` emits machine-readable stats.
    /// `--source cli|mcp|all` (default `all`) filters which telemetry files feed the dashboard.
    Usage(commands::usage::UsageArgs),
}

impl Commands {
    /// Whether this command needs a loaded graph before it can run. Exhaustive
    /// (no `_` arm) so a new variant forces a decision here at compile time,
    /// instead of silently falling through to graph-loading (or skipping it)
    /// in `main.rs`'s dispatch.
    pub fn needs_graph(&self) -> bool {
        match self {
            Commands::Inspect(_)
            | Commands::Find(_)
            | Commands::Impact(_)
            | Commands::Rename(_)
            | Commands::Cypher(_)
            | Commands::Routes(_)
            | Commands::ShapeCheck(_)
            | Commands::ToolMap(_)
            | Commands::Review(_)
            | Commands::Heuristics(_)
            | Commands::FindTransactionPatterns(_)
            | Commands::Processes(_)
            | Commands::FindSchemaBindings(_)
            | Commands::FindEventMirrors(_) => true,
            Commands::Summary(_)
            | Commands::Contracts(_)
            | Commands::Diff(_)
            | Commands::Admin { .. }
            | Commands::Dev { .. }
            | Commands::HookHandle(_)
            | Commands::HookWatcher(_)
            | Commands::Hook(_)
            | Commands::Watch(_)
            | Commands::Peers(_)
            | Commands::Group { .. }
            | Commands::Schema(_)
            | Commands::Insight(_)
            | Commands::Usage(_)
            | Commands::Uninstall(_) => false,
        }
    }

    /// This variant's `--repo` value, or `None` for variants without one
    /// (including every `needs_graph() == false` variant). Exhaustive for the
    /// same reason as `needs_graph`.
    pub fn repo(&self) -> Option<&str> {
        match self {
            Commands::Inspect(args) => args.repo.as_deref(),
            // `find --repo` doubles as a registry selector (`@all`, comma list,
            // repo name) for the bm25 fan-out; those aren't paths, and feeding
            // them to ensure_fresh as a cwd dies with "Error preparing index for
            // @all". Only treat the value as this process's repo when it's a real
            // directory; selectors resolve inside find::run_bm25. Trade-off: a
            // registry name shadowed by an identically-named local directory is
            // read as the path — path semantics win on ambiguity.
            Commands::Find(args) => args
                .repo
                .as_deref()
                .filter(|r| std::path::Path::new(r).is_dir()),
            Commands::Impact(args) => args.repo.as_deref(),
            Commands::Rename(args) => args.repo.as_deref(),
            Commands::Cypher(args) => args.repo.as_deref(),
            Commands::Routes(args) => args.repo.as_deref(),
            Commands::ShapeCheck(args) => args.repo.as_deref(),
            Commands::ToolMap(args) => args.repo.as_deref(),
            Commands::Review(args) => args.repo.as_deref(),
            Commands::Heuristics(args) => match &args.kind {
                commands::heuristics::HeuristicsKind::Saga(a) => a.repo.as_deref(),
                commands::heuristics::HeuristicsKind::SchemaBindings(a) => a.repo.as_deref(),
                commands::heuristics::HeuristicsKind::EventMirrors(a) => a.repo.as_deref(),
            },
            Commands::FindTransactionPatterns(args) => args.repo.as_deref(),
            Commands::Processes(args) => args.repo.as_deref(),
            Commands::FindSchemaBindings(args) => args.repo.as_deref(),
            Commands::FindEventMirrors(args) => args.repo.as_deref(),
            Commands::Summary(_)
            | Commands::Contracts(_)
            | Commands::Diff(_)
            | Commands::Admin { .. }
            | Commands::Dev { .. }
            | Commands::HookHandle(_)
            | Commands::HookWatcher(_)
            | Commands::Hook(_)
            | Commands::Watch(_)
            | Commands::Peers(_)
            | Commands::Group { .. }
            | Commands::Schema(_)
            | Commands::Insight(_)
            | Commands::Usage(_)
            | Commands::Uninstall(_) => None,
        }
    }
}
