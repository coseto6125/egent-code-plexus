//! `ecp admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `ecp --help` per spec §4.

use clap::Subcommand;

pub mod claude;
pub mod claude_code;
pub mod codex;
pub mod config;
pub mod drop;
pub mod gemini;
pub mod group;
pub mod index;
pub mod install_hook;
pub mod prune;
pub mod sessions;
pub(crate) mod skill_fs;
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
}

pub fn run(cmd: AdminCommands, root_cmd: clap::Command) -> Result<(), ecp_core::EcpError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
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
    }
}
