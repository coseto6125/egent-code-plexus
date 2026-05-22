//! `ecp group summary <name>` — health report for all group members.
//!
//! Delegates to `commands::summary::build_repo_health` per member; results
//! are concatenated in parallel and emitted as a flat array.
//!
//! Was `ecp group coverage` pre-rename; the `coverage` alias is kept for one
//! release for back-compat (see clap `#[command(alias = "coverage")]`).

use clap::Args;
use ecp_core::registry::{resolve_home_ecp, RegistryFile};
use ecp_core::EcpError;
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::commands::group::lookup_member;
use crate::commands::summary::build_repo_health;
use crate::repo_selector::ResolvedRepo;

#[derive(Args, Debug, Clone)]
pub struct SummaryArgs {
    /// Group name.
    pub name: String,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: SummaryArgs) -> Result<(), EcpError> {
    let home_ecp = resolve_home_ecp();
    let registry_path = home_ecp.join("registry.json");
    let reg = RegistryFile::read_or_empty(&registry_path)?;

    // 1. Validate group.
    let group = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            EcpError::InvalidArgument(format!(
                "group '{}' not found — run `ecp admin group add <repo> {}`",
                args.name, args.name
            ))
        })?;

    // 2. Resolve members to ResolvedRepo structs.
    let resolved: Vec<ResolvedRepo> = group
        .members
        .iter()
        .filter_map(|member| {
            let alias = lookup_member(&reg, member)?;
            Some(ResolvedRepo {
                dir_name: alias.dir_name.clone(),
                common_dir: alias.common_dir.clone(),
                aliases: alias.aliases.clone(),
            })
        })
        .collect();

    if resolved.is_empty() {
        if args.json {
            println!("{}", json!({ "summary": { "per_repo": [] } }));
        } else {
            eprintln!("no members in group '{}'", args.name);
        }
        return Ok(());
    }

    // 3. Fan out via rayon — pure concat, no merge.
    let per_repo: Vec<Value> = resolved
        .par_iter()
        .map(|r| build_repo_health(r, false))
        .collect();

    emit(&per_repo, args.json);
    Ok(())
}

fn emit(per_repo: &[Value], json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "summary": { "per_repo": per_repo } }))
                .unwrap_or_default()
        );
    } else {
        for repo in per_repo {
            let name = repo["repo"].as_str().unwrap_or("?");
            let freshness = repo["freshness"]["status"].as_str().unwrap_or("?");
            let nodes = repo["metrics"]["nodes"].as_u64().unwrap_or(0);
            let symbols = repo["metrics"]["symbols"].as_u64().unwrap_or(0);
            let blind = repo["blind_spots"]["total"].as_u64().unwrap_or(0);
            println!(
                "{name}: freshness={freshness} nodes={nodes} symbols={symbols} blind_spots={blind}"
            );
        }
    }
}
