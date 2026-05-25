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
    /// `blind_spots` lists only LLM-actionable opacity (dynamic-import / reflection / eval);
    /// parser-metric buckets (uid-collision / overload / ifdef-redef) live under `ecp dev uid-audit`.
    /// External-client (HTTP/DB/Redis/queue) usage detail: see `ecp tool-map`.
    Summary(commands::summary::SummaryArgs),
    /// List HTTP routes; with path, show handler + caller chain
    Routes(commands::routes::RoutesArgs),
    /// Cross-repo API contracts inventory (routes / queue / RPC)
    Contracts(commands::contracts::ContractsArgs),
    /// Edge-level resolver delta — binding tier-degradation (silent break), route / contract changes. For symbol blast-radius, use `ecp impact`.
    Diff(commands::diff::DiffArgs),

    /// Remove all ecp host integrations and optionally wipe the index cache.
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
    /// LLM-workflow audit aggregator — runs impact, summary (blind-spot),
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
    /// Surface MirrorsField heuristic edges for a SchemaField and list
    /// blind-spot candidates (cross-owner-class fields that share the name
    /// but have no mirror edge). Accepts `Class.field` or bare `field`.
    FindSchemaBindings(commands::find_schema_bindings::FindSchemaBindingsArgs),
    /// List `EventTopicMirror` heuristic edges: (publisher_fn, subscriber_fn, topic, confidence).
    /// Edges are emitted by T5-33 at confidence=0.85; filter with --min-confidence, --topic, --lib.
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
    /// List detected Process (execution-flow) nodes, or `processes trace
    /// <pattern>` to dump the full Function/Method step sequence for a
    /// matching process. Surfaces the Leiden-community + BFS detection
    /// already emitted at index time (`pass4_processes` in builder.rs).
    Processes(commands::processes::ProcessesArgs),
    /// MCP call telemetry aggregator — per-tool p50/p99/error-rate + hourly
    /// bucket counts. Reads ~/.ecp/telemetry/<repo>/calls.jsonl written by
    /// the MCP server. Schema is unstable (v1).
    Insight(commands::insight::InsightArgs),
}
