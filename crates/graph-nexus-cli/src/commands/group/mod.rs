pub mod contracts;
pub mod coverage;
pub mod extractors;
pub mod find;
pub mod impact;
pub mod matching;
pub mod search;
pub mod status;
pub mod storage;
pub mod sync;
pub mod types;

use clap::Subcommand;
use graph_nexus_core::GnxError;
use graph_nexus_core::registry::{GroupEntry, RegistryFile, RepoAlias};
use rayon::prelude::*;
use std::path::Path;

use crate::engine::Engine;
use crate::repo_selector::ResolvedRepo;

#[derive(Subcommand, Debug, Clone)]
pub enum GroupCommands {
    /// Extract contracts + run matching cascade for all group members
    Sync(sync::SyncArgs),
    /// Show staleness of each member against the last-synced meta snapshot
    Status(status::StatusArgs),
    /// List contracts with optional filtering
    Contracts(contracts::ContractsArgs),
    /// Local blast-radius for one member, fanned out via cross-repo links
    Impact(impact::ImpactArgs),
    /// BM25 search across all group members with RRF merge (or --no-merge for per-repo streams)
    Search(search::SearchArgs),
    /// BM25 symbol lookup across all group members (per-repo bucketed concat)
    Find(find::FindArgs),
    /// Health report for all group members (per-repo concat)
    Coverage(coverage::CoverageArgs),
}

pub fn run(cmd: GroupCommands) -> Result<(), GnxError> {
    match cmd {
        GroupCommands::Sync(args) => sync::run(args),
        GroupCommands::Status(args) => status::run(args),
        GroupCommands::Contracts(args) => contracts::run(args),
        GroupCommands::Impact(args) => impact::run(args),
        GroupCommands::Search(args) => search::run(args),
        GroupCommands::Find(args) => find::run(args),
        GroupCommands::Coverage(args) => coverage::run(args),
    }
}

/// Resolve a group member string to its `RepoAlias` from the registry.
/// Members are stored verbatim from `gnx admin group add <group> <repo>`;
/// `member` may be either a `dir_name` or any of the aliases. Match order:
/// (1) exact dir_name, (2) exact alias hit, (3) None (no fuzzy fallback).
///
/// Shared resolution logic — `dir_name` and `aliases` are both valid identifiers
/// for a member; no fuzzy fallback to avoid prefix-collision risk.
pub fn lookup_member<'a>(registry: &'a RegistryFile, member: &str) -> Option<&'a RepoAlias> {
    registry
        .repos
        .values()
        .find(|alias| alias.dir_name == member || alias.aliases.iter().any(|a| a == member))
}

/// Resolve each member of `group` to a loaded `Engine`, in parallel.
///
/// Returns `(display_name, Engine)` per successfully-loaded member. Members
/// that can't be resolved or whose graph fails to load are dropped silently
/// from the result vec but logged via `tracing::warn!` so the failure isn't
/// invisible. Used by `gnx group find` + `gnx group search` (both want the
/// loaded engine; coverage skips Engine entirely and resolves at the
/// `ResolvedRepo` level).
pub fn resolve_member_engines(
    group: &GroupEntry,
    registry: &RegistryFile,
    home_gnx: &Path,
) -> Vec<(String, Engine)> {
    group
        .members
        .par_iter()
        .filter_map(|member| {
            let alias = lookup_member(registry, member).or_else(|| {
                tracing::warn!("group: member '{member}' not found in registry");
                None
            })?;
            let resolved = ResolvedRepo {
                dir_name: alias.dir_name.clone(),
                common_dir: alias.common_dir.clone(),
                aliases: alias.aliases.clone(),
            };
            let graph_path = impact::latest_graph_path_for(&resolved, home_gnx).or_else(|| {
                tracing::warn!("group: no graph.bin found for '{member}'");
                None
            })?;
            let engine = match Engine::load(&graph_path) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("group: failed to load engine for '{member}': {e}");
                    return None;
                }
            };
            let display = alias
                .aliases
                .first()
                .cloned()
                .unwrap_or_else(|| member.to_string());
            Some((display, engine))
        })
        .collect()
}
