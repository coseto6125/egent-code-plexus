//! `gnx group search <name> <query>` — BM25 search across all group members.
//!
//! Default: merge results via Reciprocal Rank Fusion (RRF, K=60) and return top-K.
//! `--no-merge`: emit per-repo hit streams (legacy `--repo @group` behaviour).

use clap::Args;
use graph_nexus_core::registry::{resolve_home_gnx, RegistryFile};
use graph_nexus_core::GnxError;
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::commands::find::{self, Hit};
use crate::commands::group::impact::latest_graph_path_for;
use crate::commands::group::lookup_member;
use crate::engine::Engine;
use crate::repo_selector::ResolvedRepo;

/// Standard RRF constant — empirically tuned for dense-retrieval fusion.
const RRF_K: f64 = 60.0;

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// Group name.
    pub name: String,
    /// BM25 query string.
    pub query: String,
    /// Maximum results to return (merged mode only).
    #[arg(long, default_value_t = 5)]
    pub limit: usize,
    /// Emit per-repo streams instead of merging (disables RRF).
    #[arg(long)]
    pub no_merge: bool,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: SearchArgs) -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");
    let reg = RegistryFile::read_or_empty(&registry_path)?;

    // 1. Validate group.
    let group = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            GnxError::InvalidArgument(format!(
                "group '{}' not found — run `gnx admin group add <repo> {}`",
                args.name, args.name
            ))
        })?;

    // 2. Resolve members → (member_name, Engine).
    let engines: Vec<(String, Engine)> = group
        .members
        .iter()
        .filter_map(|member| {
            let alias = lookup_member(&reg, member)?;
            let resolved = ResolvedRepo {
                dir_name: alias.dir_name.clone(),
                common_dir: alias.common_dir.clone(),
                aliases: alias.aliases.clone(),
            };
            let graph_path = latest_graph_path_for(&resolved, &home_gnx)?;
            let engine = Engine::load(&graph_path).ok()?;
            let display = alias
                .aliases
                .first()
                .cloned()
                .unwrap_or_else(|| member.clone());
            Some((display, engine))
        })
        .collect();

    if engines.is_empty() {
        if args.json {
            println!("{}", json!({ "results": [], "per_repo": [] }));
        } else {
            eprintln!("no indexed members in group '{}'", args.name);
        }
        return Ok(());
    }

    // 3. Fan out via rayon — per-repo BM25 hits.
    let per_repo: Vec<(String, Vec<Hit>)> = engines
        .par_iter()
        .map(|(member, engine)| {
            let hits = find::run_for_repo(engine, member, &args.query, None).unwrap_or_default();
            (member.clone(), hits)
        })
        .collect();

    if args.no_merge {
        emit_no_merge(&per_repo, args.json);
    } else {
        emit_merged(&per_repo, args.limit, args.json);
    }

    Ok(())
}

// ── RRF merge ────────────────────────────────────────────────────────────────

/// A deduplicated result after RRF merge.
struct RrfHit<'a> {
    score: f64,
    hit: &'a Hit,
}

/// Reciprocal Rank Fusion: `score(uid) = Σ_repo 1 / (K + rank + 1)`.
/// `uid` here is `hit.signature` (kind + name) — a stable deduplication key
/// across repos where the same symbol may appear (e.g. shared lib symbol).
fn rrf_merge<'a>(per_repo: &'a [(String, Vec<Hit>)], limit: usize) -> Vec<RrfHit<'a>> {
    use std::collections::HashMap;
    // uid → (accumulated rrf score, first-seen Hit reference)
    let mut acc: HashMap<&str, (f64, &Hit)> = HashMap::new();

    for (_repo, hits) in per_repo {
        for (rank, hit) in hits.iter().enumerate() {
            let uid = hit.signature.as_str();
            let contrib = 1.0 / (RRF_K + rank as f64 + 1.0);
            acc.entry(uid)
                .and_modify(|(s, _)| *s += contrib)
                .or_insert((contrib, hit));
        }
    }

    let mut ranked: Vec<RrfHit> = acc
        .into_values()
        .map(|(score, hit)| RrfHit { score, hit })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit);
    ranked
}

// ── Emission ──────────────────────────────────────────────────────────────────

fn emit_merged(per_repo: &[(String, Vec<Hit>)], limit: usize, json: bool) {
    let merged = rrf_merge(per_repo, limit);

    let per_repo_summary: Vec<Value> = per_repo
        .iter()
        .map(|(repo, hits)| json!({ "repo": repo, "count": hits.len() }))
        .collect();

    if json {
        let results: Vec<Value> = merged
            .iter()
            .map(|r| {
                let h = r.hit;
                json!({
                    "repo": h.repo,
                    "name": h.name,
                    "kind": h.kind,
                    "file": h.file,
                    "line": h.line,
                    "language": h.language,
                    "rrf_score": r.score,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "results": results,
                "per_repo": per_repo_summary,
            }))
            .unwrap_or_default()
        );
    } else {
        for r in &merged {
            let h = r.hit;
            let repo_tag = h.repo.as_deref().unwrap_or("?");
            println!("[{repo_tag}] {} (rrf={:.4})", h.name, r.score);
        }
        if merged.is_empty() {
            println!("(no results)");
        }
    }
}

fn emit_no_merge(per_repo: &[(String, Vec<Hit>)], json: bool) {
    if json {
        let repo_blocks: Vec<Value> = per_repo
            .iter()
            .map(|(repo, hits)| {
                json!({
                    "repo": repo,
                    "hits": hits.iter().map(|h| json!({
                        "name": h.name,
                        "kind": h.kind,
                        "file": h.file,
                        "line": h.line,
                        "language": h.language,
                        "score": h.score,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "per_repo": repo_blocks })).unwrap_or_default()
        );
    } else {
        for (repo, hits) in per_repo {
            println!("=== {repo} ({} hits) ===", hits.len());
            for h in hits {
                println!(
                    "  [{}] {}:{} ({}) [score:{:.4} src:{}]",
                    h.kind,
                    h.file,
                    h.line,
                    h.name,
                    h.score,
                    h.score_source.as_str(),
                );
            }
        }
    }
}
