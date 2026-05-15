//! `gnx admin` subcommand namespace — registry / hooks / destructive ops.
//! Hidden from top-level `gnx --help` per spec §4.

use clap::Subcommand;

pub mod group;

#[derive(Subcommand, Debug)]
pub enum AdminCommands {
    /// Install git ref-transaction hook for branch tracking
    InstallHook(super::init::InitArgs),
    /// Delete a repo's index data + registry entry
    Drop(super::clean::CleanArgs),
    /// Remove orphan index dirs not in registry
    Prune(super::prune::PruneArgs),
    /// Rename a branch's index dir
    RenameBranch(super::rename_branch::RenameBranchArgs),
    /// Interactive TOML config editor
    Config(super::config::ConfigArgs),
    /// Manage repo group membership
    Group {
        #[command(subcommand)]
        command: group::GroupCommands,
    },
    /// Build or refresh the graph (explicit / bulk / embeddings)
    Index(super::analyze::AnalyzeArgs),
}

pub fn run(cmd: AdminCommands) -> Result<(), graph_nexus_core::GnxError> {
    match cmd {
        AdminCommands::InstallHook(args) => super::init::run(args),
        AdminCommands::Drop(args) => super::clean::run(args),
        AdminCommands::Prune(args) => super::prune::run(args),
        AdminCommands::RenameBranch(args) => super::rename_branch::run(args),
        AdminCommands::Config(args) => super::config::run(args),
        AdminCommands::Group { command } => group::run(command),
        AdminCommands::Index(args) => {
            super::analyze::run(args).map_err(graph_nexus_core::GnxError::Output)
        }
    }
}
