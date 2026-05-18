//! `gnx group impact <name> --target <sym> --repo <member>` — local impact
//! for one group member, fanned out to cross-repo links from contracts.rkyv.

use clap::Args;
use graph_nexus_core::registry::resolve_home_gnx;
use graph_nexus_core::GnxError;
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::commands::group::{lookup_member, storage};
use crate::commands::impact as local_impact;
use crate::commit_lookup::CommitIndex;
use crate::engine::Engine;
use crate::repo_selector::ResolvedRepo;

#[derive(Args, Debug, Clone)]
pub struct ImpactArgs {
    /// Group name.
    pub name: String,
    /// Symbol name (function/method/file) to analyse.
    #[arg(long)]
    pub target: String,
    /// Member name within the group (dir_name or alias).
    #[arg(long)]
    pub repo: String,
    /// Upstream (callers) or downstream (callees).
    #[arg(long, default_value = "upstream")]
    pub direction: String,
    /// Local-impact max graph traversal depth.
    #[arg(long)]
    pub max_depth: Option<u32>,
    /// Cross-repo hop depth (clamped to 1 in first wave).
    #[arg(long)]
    pub cross_depth: Option<u32>,
    /// Minimum cross-link confidence to surface.
    #[arg(long)]
    pub min_confidence: Option<f32>,
    /// Local-impact wall-clock budget in ms.
    #[arg(long)]
    pub timeout_ms: Option<u64>,
    /// Include test files in local traversal.
    #[arg(long, default_value_t = false)]
    pub include_tests: bool,
    /// JSON output instead of TOON.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: ImpactArgs) -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");
    let reg = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path)?;

    // 1. Validate group exists.
    let _group_entry = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            GnxError::InvalidArgument(format!(
                "group '{}' not found in registry\n\
                 → create it with `gnx admin group add <repo> {}`",
                args.name, args.name
            ))
        })?;

    // 2. Resolve member → RepoAlias → ResolvedRepo.
    let alias = lookup_member(&reg, &args.repo).ok_or_else(|| {
        GnxError::InvalidArgument(format!(
            "member '{}' not found in registry — check spelling or run `gnx admin index --repo <path>`",
            args.repo
        ))
    })?;

    let resolved = ResolvedRepo {
        dir_name: alias.dir_name.clone(),
        common_dir: alias.common_dir.clone(),
        aliases: alias.aliases.clone(),
    };

    // 3. Load the member's Engine.
    let cfg = graph_nexus_core::config::Config::default().group;
    let timeout_ms = args.timeout_ms.or(Some(cfg.local_impact_timeout_ms));

    let graph_path = latest_graph_path_for(&resolved, &home_gnx).ok_or_else(|| {
        GnxError::InvalidArgument(format!(
            "no indexed graph found for repo '{}' — run `gnx admin index --repo <path>` first",
            args.repo
        ))
    })?;
    let engine = Engine::load(&graph_path)
        .map_err(|e| GnxError::Io(std::io::Error::other(format!("engine load: {e}"))))?;

    // 4. Local impact.
    let local = local_impact::run_for_symbol(
        &engine,
        &alias.dir_name,
        &args.target,
        &args.direction,
        args.max_depth,
        timeout_ms,
        args.include_tests,
    )?;

    // 5. Read contracts.rkyv.
    let group_dir = storage::group_dir(&home_gnx, &args.name);
    let contract_reg = storage::read_contracts(&group_dir)
        .map_err(|e| GnxError::Io(std::io::Error::other(format!("read contracts: {e}"))))?;

    // 6. Build local_uids set.
    let local_uids: HashSet<&str> = local.direct_symbol_uids().into_iter().collect();

    // 7. Cross-depth cap (first wave = 1).
    let requested_depth = args.cross_depth.unwrap_or(cfg.cross_depth);
    let depth_cap = requested_depth.min(1);
    let cross_depth_warning = if requested_depth > 1 {
        Some(format!(
            "cross_depth clamped from {requested_depth} to 1 (first-wave limit); \
             multi-hop fan-out is not yet implemented"
        ))
    } else {
        None
    };

    // 8. Fan out through cross_links.
    let min_conf = args.min_confidence.unwrap_or(0.0);
    let cross_hits: Vec<Value> = if depth_cap >= 1 {
        contract_reg
            .cross_links
            .iter()
            .filter(|link| link.confidence >= min_conf)
            .filter(|link| {
                local_uids.contains(link.from.symbol_uid.as_str())
                    || local_uids.contains(link.to.symbol_uid.as_str())
            })
            .map(|link| {
                let match_type = match &link.match_type {
                    crate::commands::group::types::MatchType::Exact => "Exact",
                    crate::commands::group::types::MatchType::Manifest => "Manifest",
                    crate::commands::group::types::MatchType::Wildcard => "Wildcard",
                    crate::commands::group::types::MatchType::Bm25 => "Bm25",
                    crate::commands::group::types::MatchType::Embedding => "Embedding",
                };
                json!({
                    "from_repo": link.from.repo,
                    "to_repo": link.to.repo,
                    "contract_id": link.contract_id,
                    "match_type": match_type,
                    "confidence": link.confidence,
                    "from_symbol_uid": link.from.symbol_uid,
                    "to_symbol_uid": link.to.symbol_uid,
                })
            })
            .collect()
    } else {
        vec![]
    };

    // 9. Emit.
    if args.json {
        emit_json(&args, &local, &cross_hits, cross_depth_warning.as_deref());
    } else {
        emit_toon(&args, &local, &cross_hits, cross_depth_warning.as_deref());
    }

    Ok(())
}

fn emit_toon(
    args: &ImpactArgs,
    local: &local_impact::LocalImpact,
    cross_hits: &[Value],
    cross_depth_warning: Option<&str>,
) {
    println!("group         {}", args.name);
    println!("target        {}", args.target);
    println!("direct        {}", local.direct_count());
    println!("cross_hits    {}", cross_hits.len());
    for hit in cross_hits {
        let from = hit["from_repo"].as_str().unwrap_or("?");
        let to = hit["to_repo"].as_str().unwrap_or("?");
        let cid = hit["contract_id"].as_str().unwrap_or("?");
        let mt = hit["match_type"].as_str().unwrap_or("?");
        let conf = hit["confidence"].as_f64().unwrap_or(0.0);
        println!("  -> {from} → {to}: {cid} ({mt}, conf={conf:.2})");
    }
    if let Some(warn) = cross_depth_warning {
        eprintln!("note: {warn}");
    }
}

fn emit_json(
    args: &ImpactArgs,
    local: &local_impact::LocalImpact,
    cross_hits: &[Value],
    cross_depth_warning: Option<&str>,
) {
    let mut out = json!({
        "summary": {
            "group": args.name,
            "target": args.target,
            "repo": args.repo,
            "direction": args.direction,
            "direct": local.direct_count(),
            "cross_repo_hits": cross_hits.len(),
        },
        "local": local.as_json(),
        "cross": cross_hits,
        "truncated": false,
    });
    if let Some(warn) = cross_depth_warning {
        out["cross_depth_warning"] = json!(warn);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string())
    );
}

/// Resolve the latest graph.bin for a given repo, mirroring
/// `commands::coverage::latest_graph_path` — shared by group search / find /
/// coverage siblings.
pub fn latest_graph_path_for(
    r: &ResolvedRepo,
    home_gnx: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let commits_dir = home_gnx.join(&r.dir_name).join("commits");
    let idx = CommitIndex::scan(&commits_dir).ok()?;
    if idx.is_empty() {
        return None;
    }
    std::fs::read_dir(&commits_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let g = e.path().join("graph.bin");
            let mtime = std::fs::metadata(&g).ok()?.modified().ok()?;
            Some((mtime, g))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path)
}
