//! `gnx group find <name> <pattern>` — BM25 symbol lookup across all group members.
//!
//! Pure parallel concat — no merge. Each repo's bucketed results are emitted
//! with a repo column prefix.

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

#[derive(Args, Debug, Clone)]
pub struct FindArgs {
    /// Group name.
    pub name: String,
    /// BM25 pattern (symbol name or fragment).
    pub pattern: String,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: FindArgs) -> Result<(), GnxError> {
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
            println!("{}", json!({ "per_repo": [] }));
        } else {
            eprintln!("no indexed members in group '{}'", args.name);
        }
        return Ok(());
    }

    // 3. Fan out via rayon — pure concat, no merge.
    let per_repo: Vec<(String, Vec<Hit>)> = engines
        .par_iter()
        .map(|(member, engine)| {
            let hits = find::run_for_repo(engine, member, &args.pattern, None).unwrap_or_default();
            (member.clone(), hits)
        })
        .collect();

    emit(&per_repo, args.json);
    Ok(())
}

fn emit(per_repo: &[(String, Vec<Hit>)], json: bool) {
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
