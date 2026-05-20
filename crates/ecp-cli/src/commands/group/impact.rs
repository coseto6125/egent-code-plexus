//! `ecp group impact <name> --target <sym> --repo <member>` — local impact
//! for one group member, fanned out to cross-repo links from contracts.rkyv.

use clap::Args;
use ecp_core::registry::resolve_home_ecp;
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::commands::group::types::{ArchivedContractRegistry, ArchivedMatchType, MatchType};
use crate::commands::group::{lookup_member, storage};
use crate::commands::impact as local_impact;
use crate::commit_lookup::find_latest_by_mtime;
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

pub fn run(args: ImpactArgs) -> Result<(), EcpError> {
    let home_ecp = resolve_home_ecp();
    let registry_path = home_ecp.join("registry.json");
    let reg = ecp_core::registry::RegistryFile::read_or_empty(&registry_path)?;

    // 1. Validate group exists.
    let _group_entry = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            EcpError::InvalidArgument(format!(
                "group '{}' not found in registry\n\
                 → create it with `ecp admin group add <repo> {}`",
                args.name, args.name
            ))
        })?;

    // 2. Resolve member → RepoAlias → ResolvedRepo.
    let alias = lookup_member(&reg, &args.repo).ok_or_else(|| {
        EcpError::InvalidArgument(format!(
            "member '{}' not found in registry — check spelling or run `ecp admin index --repo <path>`",
            args.repo
        ))
    })?;

    let resolved = ResolvedRepo {
        dir_name: alias.dir_name.clone(),
        common_dir: alias.common_dir.clone(),
        aliases: alias.aliases.clone(),
    };

    // 3. Load the member's Engine.
    let cfg = ecp_core::config::Config::default().group;
    let timeout_ms = args.timeout_ms.or(Some(cfg.local_impact_timeout_ms));

    let graph_path = latest_graph_path_for(&resolved, &home_ecp).ok_or_else(|| {
        EcpError::InvalidArgument(format!(
            "no indexed graph found for repo '{}' — run `ecp admin index --repo <path>` first",
            args.repo
        ))
    })?;
    let engine = Engine::load(&graph_path)
        .map_err(|e| EcpError::Io(std::io::Error::other(format!("engine load: {e}"))))?;

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

    // 5. Read contracts.rkyv via zero-copy mmap.
    let group_dir = storage::group_dir(&home_ecp, &args.name);
    let contracts_path = group_dir.join(storage::CONTRACTS_FILE);

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

    // 8. Fan out through cross_links — zero-copy mmap iteration.
    let min_conf = args.min_confidence.unwrap_or(0.0);
    let cross_hits: Vec<Value> = if depth_cap >= 1 && contracts_path.exists() {
        let mmap = storage::read_contracts_archived(&group_dir)
            .map_err(|e| EcpError::Io(std::io::Error::other(format!("read contracts: {e}"))))?;
        let arch: &ArchivedContractRegistry = mmap
            .archived()
            .map_err(|e| EcpError::Io(std::io::Error::other(format!("rkyv access: {e}"))))?;
        arch.cross_links
            .iter()
            .filter(|link| link.confidence >= min_conf)
            .filter(|link| {
                local_uids.contains(link.from.symbol_uid.as_str())
                    || local_uids.contains(link.to.symbol_uid.as_str())
            })
            .map(|link| {
                let mt = match link.match_type {
                    ArchivedMatchType::Exact => MatchType::Exact,
                    ArchivedMatchType::Manifest => MatchType::Manifest,
                    ArchivedMatchType::Wildcard => MatchType::Wildcard,
                    ArchivedMatchType::Bm25 => MatchType::Bm25,
                    ArchivedMatchType::Embedding => MatchType::Embedding,
                };
                json!({
                    "from_repo": link.from.repo.as_str(),
                    "to_repo": link.to.repo.as_str(),
                    "contract_id": link.contract_id.as_str(),
                    "match_type": mt.to_string(),
                    "confidence": f32::from(link.confidence),
                    "from_symbol_uid": link.from.symbol_uid.as_str(),
                    "to_symbol_uid": link.to.symbol_uid.as_str(),
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

/// Resolve the latest graph.bin for a given repo. Delegates to
/// `commit_lookup::find_latest_by_mtime` (which also skips `.building` /
/// `.stale-*` dirs); we append `graph.bin` since that helper returns the
/// commit dir, not the archive path itself.
pub fn latest_graph_path_for(
    r: &ResolvedRepo,
    home_ecp: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let commits_dir = home_ecp.join(&r.dir_name).join("commits");
    find_latest_by_mtime(&commits_dir).map(|dir| dir.join("graph.bin"))
}
