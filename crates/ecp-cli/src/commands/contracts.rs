//! `ecp contracts` — API contracts inventory over the graph's route record.
//!
//! **Producers** are the graph's `Route` nodes (test-category files left
//! out, as in `ecp routes`). **Consumers** are its `Fetches` edges: each
//! one is a client call site whose URL matched a `Route` in the same graph.
//! A producer and a consumer pair when they share the verb and the path.
//! `scope` is `same_repo` for a consumer and a producer of one repo, and
//! `cross_repo` for two repos.
//!
//! # Limits the payload states
//!
//! - A client call to a route in another repo has no `Fetches` edge, so it is
//!   not a consumer here. `caveats` says so. `ecp group contracts` extracts
//!   client calls from source and links them across repos.
//! - Only kind `route` is extracted. `queue` and `rpc` have no extractor: a
//!   request for them alone is an error, and `--kind all` lists them in
//!   `caveats`.
//! - A repo whose graph is missing or unreadable is listed in `repo_errors`
//!   and skipped. When no repo yields a contract, the command fails: an empty
//!   `pairs` never stands for "nothing was extracted".
//!
//! # Output shape
//!
//! ```json
//! {
//!   "repos_scanned": 2,
//!   "pairs": [
//!     { "kind": "route", "method": "GET", "path": "/users/:id", "scope": "cross_repo",
//!       "producer": { "repo": "api", "file": "...", "line": 12 },
//!       "consumer": { "repo": "web", "file": "...", "line": 42 } }
//!   ],
//!   "unmatched_producer_count": 5,
//!   "unmatched_consumer_count": 2,
//!   "caveats": ["..."],
//!   "repo_errors": [{ "repo": "x", "error": "..." }]
//! }
//! ```
//!
//! With `--unmatched-only` the payload holds `unmatched_producers`,
//! `unmatched_consumers`, `caveats` and `repo_errors`.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use crate::repo_selector;
use clap::Args;
use ecp_core::contracts::identity::RouteKey;
use ecp_core::graph::{ArchivedFileCategory, ArchivedNodeKind, ArchivedRelType};
use ecp_core::registry::{resolve_home_ecp, Registry};
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Args, Debug, Clone)]
pub struct ContractsArgs {
    /// Contract kind to scan: routes / all. `queue` and `rpc` are not extracted.
    #[arg(long, default_value = "all")]
    pub kind: String,

    /// Only show contracts without a paired consumer/producer.
    #[arg(long, default_value_t = false)]
    pub unmatched_only: bool,

    /// Repository selector (path | name | @all | csv). One repo pairs its own
    /// routes with its own client calls; several repos also pair across repos.
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed
    /// toon. `json`: full-fidelity JSON. `text`: pretty-printed JSON.
    #[arg(long)]
    pub format: Option<OutputFormat>,
}

pub fn run(args: ContractsArgs) -> Result<(), EcpError> {
    let format = args.format.unwrap_or_default();

    // 1. Load registry and resolve selector.
    let home_ecp = resolve_home_ecp();
    let registry = Registry::open(&home_ecp)
        .map_err(|e| EcpError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();

    let cwd = std::env::current_dir().unwrap_or_default();
    let selector_str = args.repo.as_deref().unwrap_or("");
    let selector = repo_selector::parse(selector_str)
        .map_err(|e| EcpError::InvalidArgument(format!("--repo selector: {e}")))?;
    let resolved = repo_selector::resolve_top_level_indexing(
        &selector,
        reg,
        cwd.to_str().unwrap_or("."),
        "contracts",
    )
    .map_err(|e| EcpError::InvalidArgument(format!("--repo: {e}")))?;
    if resolved.is_empty() {
        return Err(EcpError::Output(
            "contracts: --repo resolved to no repo.\n\
             Tip: run `ecp admin config --show` to list registered repos."
                .into(),
        ));
    }

    // 2. Per-repo extraction. A repo that fails is reported, not fatal.
    let kind_filter = parse_kind_filter(&args.kind);
    if !kind_filter.contains(&"route") {
        return Err(EcpError::Output(format!(
            "contracts: kind `{}` is not implemented; only `route` contracts are extracted.\n\
             Use `--kind routes` or `--kind all`.",
            args.kind.trim()
        )));
    }
    let mut producers: Vec<Producer> = Vec::new();
    let mut consumers: Vec<Consumer> = Vec::new();
    let mut repo_errors: Vec<Value> = Vec::new();
    for repo in &resolved {
        match extract_contracts_for_repo(repo, &home_ecp) {
            Ok((mut p, mut c)) => {
                producers.append(&mut p);
                consumers.append(&mut c);
            }
            Err(e) => repo_errors.push(json!({ "repo": repo_label(repo), "error": e.to_string() })),
        }
    }
    if producers.is_empty() && consumers.is_empty() {
        return Err(EcpError::Output(format!(
            "contracts: no route contract extracted from {} repo(s).\n\
             Causes: the repo has no detected routes (`ecp routes`), or its graph is missing or unreadable.\n\
             Per-repo errors: {}",
            resolved.len(),
            if repo_errors.is_empty() {
                "none".to_string()
            } else {
                Value::Array(repo_errors).to_string()
            }
        )));
    }

    // 3. Match producers and consumers by (kind, verb, path).
    let (pairs, matched_p, matched_c) = match_pairs(&producers, &consumers);
    let unmatched_p: Vec<&Producer> = producers
        .iter()
        .enumerate()
        .filter(|(i, _)| !matched_p.contains(i))
        .map(|(_, p)| p)
        .collect();
    let unmatched_c: Vec<&Consumer> = consumers
        .iter()
        .enumerate()
        .filter(|(i, _)| !matched_c.contains(i))
        .map(|(_, c)| c)
        .collect();

    // 4. Build and emit payload.
    let mut payload = if args.unmatched_only {
        json!({
            "unmatched_producers": unmatched_p.iter().map(|p| json!({
                "kind": p.kind,
                "method": p.method,
                "path": p.path,
                "repo": p.repo,
                "file": p.file,
                "line": p.line,
            })).collect::<Vec<_>>(),
            "unmatched_consumers": unmatched_c.iter().map(|c| json!({
                "kind": c.kind,
                "method": c.method,
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
                "method": m.method,
                "path": m.path,
                "scope": if m.producer_repo == m.consumer_repo { "same_repo" } else { "cross_repo" },
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
    payload["caveats"] = json!(caveats(&kind_filter));
    if !repo_errors.is_empty() {
        payload["repo_errors"] = Value::Array(repo_errors);
    }

    emit(&payload, format)
}

fn repo_label(repo: &repo_selector::ResolvedRepo) -> String {
    repo.aliases
        .first()
        .cloned()
        .unwrap_or_else(|| repo.dir_name.clone())
}

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Producer {
    kind: String, // "route"
    method: String,
    path: String, // e.g. "/users/:id"
    repo: String,
    file: String,
    line: u32,
}

/// A client call site. `method` and `path` come from the `Route` its `Fetches`
/// edge points at.
#[derive(Debug, Clone)]
struct Consumer {
    kind: String,
    method: String,
    path: String,
    repo: String,
    file: String,
    line: u32,
}

struct Match {
    kind: String,
    method: String,
    path: String,
    producer_repo: String,
    producer_file: String,
    producer_line: u32,
    consumer_repo: String,
    consumer_file: String,
    consumer_line: u32,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map the `--kind` flag to the set of kind strings we accept.
fn parse_kind_filter(kind: &str) -> Vec<&'static str> {
    match kind.trim() {
        "routes" | "route" => vec!["route"],
        "queue" => vec!["queue"],
        "rpc" => vec!["rpc"],
        _ => vec!["route", "queue", "rpc"], // "all" or unknown → lenient
    }
}

/// What this payload cannot show, so an empty or short list is not read as
/// "no contract exists".
fn caveats(kind_filter: &[&'static str]) -> Vec<String> {
    let mut out = vec![
        "consumers are Fetches edges: a client call counts only when its URL matches a Route in \
         the same repo. A client call to a route in another repo is absent; \
         `ecp group contracts` extracts those from source."
            .to_string(),
    ];
    for kind in ["queue", "rpc"] {
        if kind_filter.contains(&kind) {
            out.push(format!(
                "kind {kind}: not implemented, no {kind} contract is listed."
            ));
        }
    }
    out
}

/// Producers (Route nodes) and consumers (Fetches edges) of one repo's latest
/// graph. `Err` when the repo has no graph or the graph cannot be read.
fn extract_contracts_for_repo(
    repo: &repo_selector::ResolvedRepo,
    home_ecp: &std::path::Path,
) -> Result<(Vec<Producer>, Vec<Consumer>), EcpError> {
    let name = repo_label(repo);
    let graph_path =
        crate::commit_lookup::latest_graph_bin(home_ecp, &repo.dir_name).ok_or_else(|| {
            EcpError::Output(format!("{name}: no graph built; run `ecp admin index`"))
        })?;
    let engine = Engine::load(&graph_path)
        .map_err(|e| EcpError::Output(format!("{name}: graph load failed: {e}")))?;
    let graph = engine
        .graph()
        .map_err(|e| EcpError::Rkyv(format!("{name}: {e}")))?;

    let is_test = |node: &ecp_core::graph::ArchivedNode| {
        let file = &graph.files[node.file_idx.to_native() as usize];
        matches!(file.category, ArchivedFileCategory::Test)
    };
    let file_of = |node: &ecp_core::graph::ArchivedNode| {
        graph.files[node.file_idx.to_native() as usize]
            .path
            .resolve(&graph.string_pool)
            .to_string()
    };
    let route_of = |idx: usize| {
        let node = graph.nodes.get(idx)?;
        if !matches!(node.kind, ArchivedNodeKind::Route) {
            return None;
        }
        let (verb, path) = RouteKey::split_name(node.name.resolve(&graph.string_pool))?;
        Some((verb.to_string(), path.to_string()))
    };

    let mut producers = Vec::new();
    for (idx, node) in graph.nodes.iter().enumerate() {
        if !matches!(node.kind, ArchivedNodeKind::Route) || is_test(node) {
            continue;
        }
        let Some((method, path)) = route_of(idx) else {
            continue;
        };
        producers.push(Producer {
            kind: "route".into(),
            method,
            path,
            repo: name.clone(),
            file: file_of(node),
            line: node.start_line(),
        });
    }

    let mut consumers = Vec::new();
    for edge in graph.edges.iter() {
        if !matches!(edge.rel_type, ArchivedRelType::Fetches) {
            continue;
        }
        let Some(source) = graph.nodes.get(edge.source.to_native() as usize) else {
            continue;
        };
        let Some((method, path)) = route_of(edge.target.to_native() as usize) else {
            continue;
        };
        if is_test(source) {
            continue;
        }
        consumers.push(Consumer {
            kind: "route".into(),
            method,
            path,
            repo: name.clone(),
            file: file_of(source),
            line: source.start_line(),
        });
    }
    Ok((producers, consumers))
}

/// A verb pairs with itself and with `ANY` on either side.
fn verbs_meet(a: &str, b: &str) -> bool {
    a == b || a == "ANY" || b == "ANY"
}

/// Pair each producer with every consumer that shares (kind, verb, path). One
/// producer can match several consumers. Returns the pairs and the indices of
/// the matched producers and consumers.
fn match_pairs(
    producers: &[Producer],
    consumers: &[Consumer],
) -> (Vec<Match>, HashSet<usize>, HashSet<usize>) {
    let mut pairs = Vec::new();
    let mut matched_p = HashSet::new();
    let mut matched_c = HashSet::new();
    for (pi, p) in producers.iter().enumerate() {
        for (ci, c) in consumers.iter().enumerate() {
            if p.kind == c.kind && p.path == c.path && verbs_meet(&p.method, &c.method) {
                pairs.push(Match {
                    kind: p.kind.clone(),
                    method: c.method.clone(),
                    path: p.path.clone(),
                    producer_repo: p.repo.clone(),
                    producer_file: p.file.clone(),
                    producer_line: p.line,
                    consumer_repo: c.repo.clone(),
                    consumer_file: c.file.clone(),
                    consumer_line: c.line,
                });
                matched_p.insert(pi);
                matched_c.insert(ci);
            }
        }
    }
    (pairs, matched_p, matched_c)
}
