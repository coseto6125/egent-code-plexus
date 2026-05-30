//! `ecp admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `ecp --help` per spec §4.

use clap::{Args, Subcommand};

pub mod claude;
pub mod claude_code;
pub mod codex;
pub mod config;
pub mod doctor;
pub mod drop;
pub mod gc;
pub mod gemini;
pub mod group;
pub mod index;
pub mod install_hook;
pub mod prune;
pub mod sessions;
pub(crate) mod skill_fs;
pub(crate) mod skill_source;
pub mod update_check;
#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook for branch tracking (or Claude Code hooks with --claude-code)
    InstallHook(install_hook::InstallHookArgs),
    /// Remove Claude Code hook entries from settings.json
    UninstallHook(claude_code::UninstallHookArgs),
    /// Show Claude Code hook install status
    Status(claude_code::StatusArgs),
    /// Delete a repo's index data + registry entry
    Drop(drop::DropArgs),
    /// Remove orphan index dirs not in registry
    Prune(prune::PruneArgs),
    /// Garbage-collect stale graph generations + retired repo/session dirs
    Gc(gc::GcArgs),
    /// Interactive TOML config editor
    Config(config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph (explicit / bulk)
    Index(index::IndexArgs),
    /// List / inspect L1 sessions
    Sessions {
        #[command(subcommand)]
        command: sessions::SessionsCommand,
    },
    /// Scriptable Claude Code host integration commands
    Claude {
        #[command(subcommand)]
        command: claude::ClaudeCommands,
    },
    /// Scriptable Codex host integration commands
    Codex {
        #[command(subcommand)]
        command: codex::CodexCommands,
    },
    /// Scriptable Gemini host integration commands
    Gemini {
        #[command(subcommand)]
        command: gemini::GeminiCommands,
    },
    /// Run MCP server (serve) or list exposed tools (tools).
    Mcp(crate::commands::mcp::McpArgs),
    /// Environment health check: skills / index / host / config / registry / version. `--fix` repairs fixable items.
    Doctor(doctor::DoctorArgs),
    /// Background-only: throttled daily probe for a newer ecp release. Spawned by the session_start hook; never run manually.
    #[command(hide = true)]
    CheckUpdate,
    /// List indexed repos (alias for `ecp summary` registry overview; no `--repo` / `--detailed`).
    ListRepos(ListReposArgs),
}

/// Args for `ecp admin list-repos` — narrowed alias of `ecp summary`.
///
/// Only the output `--format` is forwarded. The repo-selector and detailed
/// flags are intentionally suppressed so the verb means exactly "list the
/// registry overview"; for per-repo health, the user runs `ecp summary --repo …`.
#[derive(Args, Debug, Clone)]
pub struct ListReposArgs {
    /// Output format. Same semantics as `ecp summary --format`
    /// (default LLM-tuned toon; `toon` / `json` / `text` accepted).
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(cmd: AdminCommands, root_cmd: clap::Command) -> Result<(), ecp_core::EcpError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
        AdminCommands::Gc(args) => gc::run(args),
        AdminCommands::Config(args) => config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => index::run(args).map_err(ecp_core::EcpError::Output),
        AdminCommands::Sessions { command } => {
            sessions::run(command).map_err(ecp_core::EcpError::Output)
        }
        AdminCommands::Claude { command } => claude::run(command),
        AdminCommands::Codex { command } => codex::run(command),
        AdminCommands::Gemini { command } => gemini::run(command),
        AdminCommands::Mcp(args) => crate::commands::mcp::run(args, root_cmd),
        AdminCommands::Doctor(args) => doctor::run(args),
        AdminCommands::CheckUpdate => update_check::run(),
        AdminCommands::ListRepos(args) => {
            // Forward to `ecp summary` with `--repo` cleared so the alias
            // always emits the registry-level overview, not per-repo health.
            let summary_args = crate::commands::summary::SummaryArgs {
                repo: None,
                detailed: false,
                format: args.format,
            };
            // `summary::run` ignores its graph arg when no `--repo` is given
            // (registry overview path), so the default graph path is fine.
            let default_graph = std::path::PathBuf::from(".ecp/graph.bin");
            crate::commands::summary::run(summary_args, &default_graph)
        }
    }
}
