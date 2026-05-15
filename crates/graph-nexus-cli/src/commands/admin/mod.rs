//! `gnx admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `gnx --help` per spec §4.

use clap::Subcommand;

pub mod claude_code;
pub mod config;
pub mod drop;
pub mod group;
pub mod index;
pub mod install_hook;
pub mod prune;
pub mod rename_branch;

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
    /// Rename a branch's index dir
    RenameBranch(rename_branch::RenameBranchArgs),
    /// Interactive TOML config editor
    Config(config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph (explicit / bulk / embeddings)
    Index(index::IndexArgs),
    /// Run MCP server (serve) or list exposed tools (tools).
    Mcp(crate::commands::mcp::McpArgs),
    /// Diff resolver dump against language oracle (gnx-dev QA)
    VerifyResolver(crate::commands::verify_resolver::VerifyResolverArgs),
}

pub fn run(cmd: AdminCommands, root_cmd: clap::Command) -> Result<(), graph_nexus_core::GnxError> {
    match cmd {
        AdminCommands::InstallHook(args) => install_hook::run(args),
        AdminCommands::UninstallHook(args) => claude_code::run_uninstall(args),
        AdminCommands::Status(args) => claude_code::run_status(args),
        AdminCommands::Drop(args) => drop::run(args),
        AdminCommands::Prune(args) => prune::run(args),
        AdminCommands::RenameBranch(args) => rename_branch::run(args),
        AdminCommands::Config(args) => config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => index::run(args).map_err(graph_nexus_core::GnxError::Output),
        AdminCommands::Mcp(args) => crate::commands::mcp::run(args, root_cmd),
        AdminCommands::VerifyResolver(args) => crate::commands::verify_resolver::run(args),
    }
}
