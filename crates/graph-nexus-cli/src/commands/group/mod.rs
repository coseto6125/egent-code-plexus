pub mod extractors;
pub mod matching;
pub mod status;
pub mod storage;
pub mod sync;
pub mod types;

use clap::Subcommand;
use graph_nexus_core::GnxError;
use graph_nexus_core::registry::{RegistryFile, RepoAlias};

#[derive(Subcommand, Debug, Clone)]
pub enum GroupCommands {
    /// Extract contracts + run matching cascade for all group members
    Sync(sync::SyncArgs),
    /// Show staleness of each member against the last-synced meta snapshot
    Status(status::StatusArgs),
}

pub fn run(cmd: GroupCommands) -> Result<(), GnxError> {
    match cmd {
        GroupCommands::Sync(args) => sync::run(args),
        GroupCommands::Status(args) => status::run(args),
    }
}

/// Resolve a group member string to its `RepoAlias` from the registry.
/// Members are stored verbatim from `gnx admin group add <group> <repo>`;
/// `member` may be either a `dir_name` or any of the aliases. Match order:
/// (1) exact dir_name, (2) exact alias hit, (3) None (no fuzzy fallback).
///
/// Used by T9 sync + T10 status + T12 impact — keep the logic single-sourced.
pub fn lookup_member<'a>(registry: &'a RegistryFile, member: &str) -> Option<&'a RepoAlias> {
    registry
        .repos
        .values()
        .find(|alias| alias.dir_name == member || alias.aliases.iter().any(|a| a == member))
}
