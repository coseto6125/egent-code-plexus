//! `cgn contracts` — cross-repo API contracts inventory.
//!
//! Scans ≥2 registered repos and matches **producers** (HTTP routes,
//! queue producers, RPC endpoints) to **consumers** (HTTP clients,
//! queue consumers, RPC callers).
//!
//! Designed for `--repo @<group>` / `@all` workflows; errors out
//! gracefully when fewer than 2 repos are resolved.
//!
//! # Output shape
//!
//! Default (`--unmatched-only` not set):
//! ```json
//! {
//!   "repos_scanned": 3,
//!   "pairs": [
//!     { "kind": "route", "path": "/users/:id",
//!       "producer": { "repo": "api", "file": "...", "line": 12 },
//!       "consumer": { "repo": "web", "file": "...", "line": 42 } }
//!   ],
//!   "unmatched_producer_count": 5,
//!   "unmatched_consumer_count": 2
//! }
//! ```
//!
//! With `--unmatched-only`:
//! ```json
//! {
//!   "unmatched_producers": [...],
//!   "unmatched_consumers": [...]
//! }
//! ```
//!
//! # Producer / consumer extraction (v1 stub)
//!
//! Full extraction from Route nodes (`NodeKind::Route`) and FETCHES edges
//! (`RelType::Fetches`) is deferred — see `extract_contracts_for_repo`.
//! The architectural shape, multi-repo gate, matching algorithm, and
//! payload structure are fully wired; `pairs[]` will populate once the
//! per-repo extraction is ported from prior implementations of Route-node
//! iteration and FETCHES-edge traversal (see git history for the deleted
//! commands/api_impact.rs and commands/tool_map.rs).

use crate::output::{emit, OutputFormat};
use crate::repo_selector;
use clap::Args;
use cgn_core::registry::{resolve_home_gnx, Registry};
use cgn_core::GnxError;
use serde_json::json;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    /// Contract kind to scan: routes / queue / rpc / all.
    #[arg(long, default_value = "all")]
    pub kind: String,

    /// Only show contracts without a paired consumer/producer.
    #[arg(long, default_value_t = false)]
    pub unmatched_only: bool,

    /// Repository selector (path | name | @group | @all | csv).
    /// Requires ≥2 repos to be meaningful.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: toon (default) | json.
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: ContractsArgs) -> Result<(), GnxError> {
    let format = OutputFormat::parse(args.format.as_deref());

    // 1. Load registry and resolve selector.
    let home_gnx = resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();

    let cwd = std::env::current_dir().unwrap_or_default();
    let selector_str = args.repo.as_deref().unwrap_or("");
    let selector = repo_selector::parse(selector_str)
        .map_err(|e| GnxError::InvalidArgument(format!("--repo selector: {e}")))?;
    let resolved =
        repo_selector::resolve_top_level(&selector, reg, cwd.to_str().unwrap_or("."), "contracts")
            .map_err(|e| GnxError::InvalidArgument(format!("--repo: {e}")))?;

    // 2. Multi-repo gate: contracts is only meaningful across ≥2 repos.
    if resolved.len() < 2 {
        return Err(GnxError::Output(format!(
            "contracts requires ≥2 repos for cross-repo matching (got {n}).\n\
             Use --repo @<group-with-≥2-members> or --repo @all.\n\
             Tip: run `cgn admin config --show` to list registered repos.",
            n = resolved.len()
        )));
    }

    // 3. Per-repo extraction: producers + consumers.
    let kind_filter = parse_kind_filter(&args.kind);
    let mut producers: Vec<Producer> = Vec::new();
    let mut consumers: Vec<Consumer> = Vec::new();

    for repo in &resolved {
        // Best-effort: skip repos whose extraction fails; don't abort.
        let (mut p, mut c) =
            extract_contracts_for_repo(repo, &kind_filter).unwrap_or_else(|_| (vec![], vec![]));
        producers.append(&mut p);
        consumers.append(&mut c);
    }

    // 4. Match producers ↔ consumers by (kind, path), cross-repo only.
    let pairs = match_pairs(&producers, &consumers);

    let unmatched_p: Vec<&Producer> = producers
        .iter()
        .filter(|p| !pairs.iter().any(|m| m.producer_key() == p.key()))
        .collect();
    let unmatched_c: Vec<&Consumer> = consumers
        .iter()
        .filter(|c| !pairs.iter().any(|m| m.consumer_key() == c.key()))
        .collect();

    // 5. Build and emit payload.
    let payload = if args.unmatched_only {
        json!({
            "unmatched_producers": unmatched_p.iter().map(|p| json!({
                "kind": p.kind,
                "path": p.path,
                "repo": p.repo,
                "file": p.file,
                "line": p.line,
            })).collect::<Vec<_>>(),
            "unmatched_consumers": unmatched_c.iter().map(|c| json!({
                "kind": c.kind,
                "path": c.path,
                "repo": c.repo,
                "file": c.file,
                "line": c.line,
            })).collect::<Vec<_>>(),
        })
    } else {
        json!({
            "repos_scanned": resolved.len(),
            "pairs": pairs.iter().map(|m| json!({
                "kind": m.kind,
                "path": m.path,
                "producer": {
                    "repo": m.producer_repo,
                    "file": m.producer_file,
                    "line": m.producer_line,
                },
                "consumer": {
                    "repo": m.consumer_repo,
                    "file": m.consumer_file,
                    "line": m.consumer_line,
                },
            })).collect::<Vec<_>>(),
            "unmatched_producer_count": unmatched_p.len(),
            "unmatched_consumer_count": unmatched_c.len(),
        })
    };

    emit(&payload, format)
}

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Producer {
    kind: String, // "route" | "queue" | "rpc"
    path: String, // e.g. "/users/:id" for routes, topic name for queue
    repo: String,
    file: String,
    line: u32,
}

impl Producer {
    fn key(&self) -> (&str, &str) {
        (&self.kind, &self.path)
    }
}

#[derive(Debug, Clone)]
struct Consumer {
    kind: String,
    path: String,
    repo: String,
    file: String,
    line: u32,
}

impl Consumer {
    fn key(&self) -> (&str, &str) {
        (&self.kind, &self.path)
    }
}

struct Match {
    kind: String,
    path: String,
    producer_repo: String,
    producer_file: String,
    producer_line: u32,
    consumer_repo: String,
    consumer_file: String,
    consumer_line: u32,
}

impl Match {
    fn producer_key(&self) -> (&str, &str) {
        (&self.kind, &self.path)
    }
    fn consumer_key(&self) -> (&str, &str) {
        (&self.kind, &self.path)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map the `--kind` flag to the set of kind strings we accept.
fn parse_kind_filter(kind: &str) -> Vec<&'static str> {
    match kind {
        "routes" | "route" => vec!["route"],
        "queue" => vec!["queue"],
        "rpc" => vec!["rpc"],
        _ => vec!["route", "queue", "rpc"], // "all" or unknown → lenient
    }
}

/// Extract producers and consumers for a single repo.
///
/// # Implementation roadmap
///
/// To turn this into live extraction:
///   1. Load `{repo.index_dir_root}/<branch>/graph.bin` via `Engine::load`.
///   2. Iterate graph nodes with `NodeKind::Route` to emit producers.
///   3. Iterate edges with `RelType::Fetches` to emit consumers.
///   4. Filter by `kind_filter` and apply `repo.name` for cross-repo matching.
///
/// Reference patterns are preserved in git history (pre-T5.1 commits
/// for `commands/api_impact.rs` and `commands/tool_map.rs`).
fn extract_contracts_for_repo(
    _repo: &repo_selector::ResolvedRepo,
    _kind_filter: &[&'static str],
) -> Result<(Vec<Producer>, Vec<Consumer>), GnxError> {
    Ok((vec![], vec![]))
}

/// Pair each producer with every consumer that shares (kind, path) but
/// lives in a different repo. One producer can match multiple consumers.
fn match_pairs(producers: &[Producer], consumers: &[Consumer]) -> Vec<Match> {
    let mut pairs = Vec::new();
    for p in producers {
        for c in consumers {
            if p.kind == c.kind && p.path == c.path && p.repo != c.repo {
                pairs.push(Match {
                    kind: p.kind.clone(),
                    path: p.path.clone(),
                    producer_repo: p.repo.clone(),
                    producer_file: p.file.clone(),
                    producer_line: p.line,
                    consumer_repo: c.repo.clone(),
                    consumer_file: c.file.clone(),
                    consumer_line: c.line,
                });
            }
        }
    }
    pairs
}
