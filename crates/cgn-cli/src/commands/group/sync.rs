//! `cgn group sync <name>` — extract contracts from all group members,
//! run the matching cascade, and write contracts.rkyv + meta.json atomically.

use clap::Args;
use cgn_core::registry::{resolve_home_cgn, RegistryFile};
use cgn_core::CgnError;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use crate::commands::group::extractors;
use crate::commands::group::matching::match_contracts;
use crate::commands::group::storage::{
    self, write_contracts, write_meta, GroupMeta, RepoSnapshot,
};
use crate::commands::group::types::{ContractRegistry, CrossLink, MatchType, StoredContract};
use crate::commands::group::lookup_member;

/// Per-member sync outcome. `Err` carries (member-name, reason) so the caller
/// can record the bare member name in `missing_repos` while logging the full
/// reason — without the consumer having to re-parse the error string.
type MemberResult = Result<(String, Vec<StoredContract>, RepoSnapshot), (String, String)>;

#[derive(Args, Debug, Clone)]
pub struct SyncArgs {
    /// Group name (must exist in registry.json).
    pub name: String,
    /// Skip BM25 stage; exact match only.
    #[arg(long)]
    pub exact_only: bool,
    /// Don't bail when per-repo index is stale.
    #[arg(long)]
    pub allow_stale: bool,
    /// Emit JSON instead of TOON.
    #[arg(long)]
    pub json: bool,
    /// Show per-cross-link detail.
    #[arg(long)]
    pub verbose: bool,
}

pub fn run(args: SyncArgs) -> Result<(), CgnError> {
    let start = Instant::now();

    // 1. Resolve home, open registry.
    let home_cgn = resolve_home_cgn();
    let registry_path = home_cgn.join("registry.json");
    let reg = RegistryFile::read_or_empty(&registry_path)?;

    // 2. Look up the group.
    let group_entry = reg
        .groups
        .iter()
        .find(|g| g.name == args.name)
        .ok_or_else(|| {
            CgnError::InvalidArgument(format!(
                "group '{}' not found in registry\n\
                 → create it with `cgn admin group add <repo> {}`",
                args.name, args.name
            ))
        })?
        .clone();

    // 3. Load config (default if no project root / file missing). The group
    //    config lives in the global config, not per-repo, so we use defaults.
    let cfg = cgn_core::config::Config::default().group;

    // 4. Resolve each member → source path via registry. Members are stored
    //    as the alias string passed to `cgn admin group add`, which may be
    //    either a dir_name or an alias. Resolve by matching both.
    let all_extractors = extractors::registry();

    let member_results: Vec<MemberResult> = group_entry
            .members
            .par_iter()
            .map(|member| extract_member(member, &reg, &all_extractors, args.allow_stale))
            .collect();

    let mut all_contracts: Vec<StoredContract> = Vec::new();
    let mut repo_snapshots: BTreeMap<String, RepoSnapshot> = BTreeMap::new();
    let mut missing_repos: Vec<String> = Vec::new();

    for result in member_results {
        match result {
            Ok((repo_name, contracts, snapshot)) => {
                all_contracts.extend(contracts);
                repo_snapshots.insert(repo_name, snapshot);
            }
            Err((member, reason)) => {
                tracing::warn!("group sync: skipping member '{member}' — {reason}");
                missing_repos.push(member);
            }
        }
    }

    // 5. Match contracts.
    let group_dir = storage::group_dir(&home_cgn, &args.name);
    let (cross_links, unmatched) =
        match_contracts(&all_contracts, &group_dir, &cfg, args.exact_only)?;

    // 6. Write outputs atomically.
    let contract_registry = ContractRegistry {
        version: 1,
        contracts: all_contracts.clone(),
        cross_links: cross_links.clone(),
        unmatched: unmatched.clone(),
    };
    write_contracts(&group_dir, &contract_registry)?;

    let now = chrono_now();
    let meta = GroupMeta {
        version: 1,
        generated_at: now,
        repo_snapshots,
        missing_repos,
        config_source: "default".to_string(),
    };
    write_meta(&group_dir, &meta)?;

    // 7. Emit summary.
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let exact_count = cross_links
        .iter()
        .filter(|l| matches!(l.match_type, MatchType::Exact))
        .count();
    let bm25_count = cross_links
        .iter()
        .filter(|l| matches!(l.match_type, MatchType::Bm25))
        .count();

    let summary = SyncSummary {
        name: &args.name,
        contracts: all_contracts.len(),
        exact: exact_count,
        bm25: bm25_count,
        unmatched: unmatched.len(),
        elapsed_ms,
        verbose: args.verbose,
        links: &cross_links,
    };
    if args.json {
        summary.emit_json();
    } else {
        summary.emit_toon();
    }

    Ok(())
}

/// Walk member's source tree, run all matching extractors, return contracts + snapshot.
fn extract_member(
    member: &str,
    reg: &RegistryFile,
    all_extractors: &[extractors::ExtractorEntry],
    _allow_stale: bool,
) -> Result<(String, Vec<StoredContract>, RepoSnapshot), (String, String)> {
    let m = member.to_string();
    // Resolve member name → RepoAlias via shared helper (dir_name or alias, no fuzzy).
    let alias = lookup_member(reg, member).ok_or_else(|| {
        (
            m.clone(),
            "not found in registry (run `cgn admin index <path>` first)".to_string(),
        )
    })?;

    // Derive the source root from common_dir. common_dir is the `.git` dir;
    // the source root is its parent (for non-bare repos).
    let common_dir = std::path::PathBuf::from(&alias.common_dir);
    let src_root = common_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(common_dir.clone());

    if !src_root.exists() {
        return Err((
            m,
            format!("source root does not exist: {}", src_root.display()),
        ));
    }

    // Capture last commit via `git rev-parse HEAD`.
    let last_commit = git_head(&src_root).unwrap_or_else(|_| "unknown".to_string());

    // Walk files, run extractors per language.
    let contracts = walk_and_extract(&src_root, member, all_extractors);

    let snapshot = RepoSnapshot {
        indexed_at: chrono_now(),
        last_commit,
    };

    Ok((member.to_string(), contracts, snapshot))
}

/// Walk `src_root`, for each file whose extension maps to a supported lang,
/// run all extractors for that lang and collect `StoredContract`s.
fn walk_and_extract(
    src_root: &Path,
    repo_name: &str,
    all_extractors: &[extractors::ExtractorEntry],
) -> Vec<StoredContract> {
    use walkdir::WalkDir;

    let mut out: Vec<StoredContract> = Vec::new();

    for entry in WalkDir::new(src_root)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let Some(lang) = extractors::lang_for_extension(ext) else {
            continue;
        };

        let source = match std::fs::read(path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("group sync: failed to read {}: {e}", path.display());
                continue;
            }
        };

        for extractor in all_extractors.iter().filter(|e| e.lang == lang) {
            for contract in (extractor.extract)(path, &source) {
                out.push(StoredContract {
                    repo: repo_name.to_string(),
                    inner: contract,
                });
            }
        }
    }

    out
}

fn git_head(repo_root: &Path) -> std::io::Result<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("git rev-parse HEAD failed"));
    }
    Ok(std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .trim()
        .to_string())
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

struct SyncSummary<'a> {
    name: &'a str,
    contracts: usize,
    exact: usize,
    bm25: usize,
    unmatched: usize,
    elapsed_ms: u64,
    verbose: bool,
    links: &'a [CrossLink],
}

impl SyncSummary<'_> {
    fn emit_toon(&self) {
        let Self { name, contracts, exact, bm25, unmatched, elapsed_ms, .. } = self;
        println!("group         {name}");
        println!("contracts     {contracts}");
        println!("cross_links");
        println!("  exact       {exact}");
        println!("  bm25        {bm25}");
        println!("unmatched     {unmatched}");
        println!("elapsed_ms    {elapsed_ms}");
        if self.verbose {
            for link in self.links {
                println!(
                    "  {} -> {}  [{}, conf={:.2}]  {}",
                    link.from.repo, link.to.repo, link.match_type,
                    link.confidence, link.contract_id
                );
            }
        }
    }

    fn emit_json(&self) {
        let Self { name, contracts, exact, bm25, unmatched, elapsed_ms, .. } = self;
        if self.verbose {
            let link_arr: Vec<_> = self.links
                .iter()
                .map(|l| format!(
                    r#"{{"from":"{}","to":"{}","match_type":"{}","confidence":{:.2},"contract_id":"{}"}}"#,
                    l.from.repo, l.to.repo, l.match_type, l.confidence, l.contract_id
                ))
                .collect();
            println!(
                r#"{{"group":"{name}","contracts":{contracts},"cross_links":{{"exact":{exact},"bm25":{bm25}}},"unmatched":{unmatched},"elapsed_ms":{elapsed_ms},"links":[{}]}}"#,
                link_arr.join(",")
            );
        } else {
            println!(
                r#"{{"group":"{name}","contracts":{contracts},"cross_links":{{"exact":{exact},"bm25":{bm25}}},"unmatched":{unmatched},"elapsed_ms":{elapsed_ms}}}"#
            );
        }
    }
}

