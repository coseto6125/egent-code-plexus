//! `cgn group find <name> <pattern>` — BM25 symbol lookup across all group
//! members. Single verb covers both per-repo bucketed concat (`--merge none`,
//! default) and cross-repo RRF-merged top-K (`--merge rrf`).

use clap::{Args, ValueEnum};
use cgn_core::registry::{resolve_home_gnx, RegistryFile};
use cgn_core::GnxError;
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::commands::find::{self, Hit};
use crate::commands::group::resolve_member_engines;
use crate::engine::Engine;

/// Standard RRF constant — empirically tuned for dense-retrieval fusion.
const RRF_K: f64 = 60.0;
/// Default top-K when `--merge rrf` is set without an explicit `--limit`.
const DEFAULT_RRF_LIMIT: usize = 5;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[value(rename_all = "lowercase")]
pub enum MergeMode {
    /// Per-repo bucketed concat — every member's hits emitted under its own
    /// header. Matches the single-repo `cgn find --mode bm25` shape.
    None,
    /// Reciprocal Rank Fusion across repos → unified top-K. Ranking is by
    /// `Σ_repo 1 / (RRF_K + rank + 1)` over `Hit.signature` as the dedupe key.
    Rrf,
}

#[derive(Args, Debug, Clone)]
pub struct FindArgs {
    /// Group name.
    pub name: String,
    /// BM25 pattern (symbol name or fragment). Required unless `--batch`.
    #[arg(required_unless_present = "batch")]
    pub pattern: Option<String>,
    /// Result assembly mode.
    #[arg(long, value_enum, default_value_t = MergeMode::None)]
    pub merge: MergeMode,
    /// Top-K results (only meaningful with `--merge rrf`; rejected otherwise).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Read patterns from stdin, one per line. Lines starting with `#` or
    /// empty after trim are skipped. Each pattern emits a `=== pattern: <p> ===`
    /// divider so downstream scripts can split per-query.
    #[arg(long)]
    pub batch: bool,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: FindArgs) -> Result<(), GnxError> {
    // Cross-flag validation: `--limit` is RRF-only. Single-mode (`--merge none`)
    // emits per-repo concat where a global cap would be ambiguous (truncate
    // which repo first?), so reject the combo loudly.
    if args.limit.is_some() && args.merge != MergeMode::Rrf {
        return Err(GnxError::InvalidArgument(
            "--limit requires `--merge rrf`; per-repo concat (default) has no global top-K".into(),
        ));
    }

    let home_gnx = resolve_home_gnx();
    let registry_path = home_gnx.join("registry.json");
    let reg = RegistryFile::read_or_empty(&registry_path)?;

    let group = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            GnxError::InvalidArgument(format!(
                "group '{}' not found — run `cgn admin group add <repo> {}`",
                args.name, args.name
            ))
        })?;

    let engines: Vec<(String, Engine)> = resolve_member_engines(group, &reg, &home_gnx);

    if engines.is_empty() {
        emit_empty(args.merge, args.json);
        return Ok(());
    }

    if args.batch {
        run_batch(&engines, args.merge, args.limit, args.json);
    } else {
        let pattern = args.pattern.as_deref().expect("clap required_unless_present");
        run_one(&engines, pattern, args.merge, args.limit, args.json);
    }
    Ok(())
}

// ── Per-pattern dispatch ──────────────────────────────────────────────────────

fn run_one(
    engines: &[(String, Engine)],
    pattern: &str,
    mode: MergeMode,
    limit: Option<usize>,
    json: bool,
) {
    let per_repo = fan_out_per_repo(engines, pattern);
    match mode {
        MergeMode::None => emit_concat(&per_repo, json),
        MergeMode::Rrf => emit_rrf(&per_repo, limit, json),
    }
}

fn run_batch(engines: &[(String, Engine)], mode: MergeMode, limit: Option<usize>, json: bool) {
    use std::io::BufRead;

    let stdin = std::io::stdin();
    let patterns: Vec<String> = stdin
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter_map(|s| {
            let t = s.trim();
            (!t.is_empty() && !t.starts_with('#')).then(|| t.to_string())
        })
        .collect();

    if patterns.is_empty() {
        eprintln!("→ batch: no patterns on stdin (one per line, `#` for comments)");
        return;
    }

    for pattern in &patterns {
        println!("=== pattern: {pattern} ===");
        run_one(engines, pattern, mode, limit, json);
    }
}

fn fan_out_per_repo<'a>(
    engines: &'a [(String, Engine)],
    pattern: &str,
) -> Vec<(String, Vec<Hit>)> {
    engines
        .par_iter()
        .map(|(member, engine)| {
            let hits = find::run_for_repo(engine, member, pattern, None).unwrap_or_default();
            (member.clone(), hits)
        })
        .collect()
}

// ── RRF merge ─────────────────────────────────────────────────────────────────

struct RrfHit<'a> {
    score: f64,
    hit: &'a Hit,
}

/// `score(uid) = Σ_repo 1 / (RRF_K + rank + 1)`. `uid` is `hit.signature`
/// (kind + name) — a stable cross-repo dedupe key for the same symbol
/// appearing in multiple members (e.g. shared lib).
fn rrf_merge<'a>(per_repo: &'a [(String, Vec<Hit>)], limit: usize) -> Vec<RrfHit<'a>> {
    use std::collections::HashMap;
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

fn emit_empty(mode: MergeMode, json: bool) {
    if json {
        match mode {
            MergeMode::None => println!("{}", json!({ "per_repo": [] })),
            MergeMode::Rrf => println!("{}", json!({ "results": [], "per_repo": [] })),
        }
    } else {
        eprintln!("no indexed members in group");
    }
}

fn emit_concat(per_repo: &[(String, Vec<Hit>)], json: bool) {
    if json {
        let repo_blocks: Vec<Value> = per_repo
            .iter()
            .map(|(repo, hits)| {
                json!({
                    "repo": repo,
                    "count": hits.len(),
                    "hits": hits.iter().map(|h| json!({
                        "repo": repo,
                        "name": h.name,
                        "kind": h.kind,
                        "file": h.file,
                        "line": h.line,
                        "language": h.language,
                        "score": h.score,
                        "score_source": h.score_source.as_str(),
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
                    "  [{repo}] [{}] {}:{} ({}) [score:{:.4} src:{}]",
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

fn emit_rrf(per_repo: &[(String, Vec<Hit>)], limit: Option<usize>, json: bool) {
    let merged = rrf_merge(per_repo, limit.unwrap_or(DEFAULT_RRF_LIMIT));
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
