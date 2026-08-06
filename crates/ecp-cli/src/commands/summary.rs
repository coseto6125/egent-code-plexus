//! `ecp summary` — unified registry + repo health entry point.
//!
//! Folds doctor + status + list + summarize into one command:
//!
//! - No `--repo`, cwd is indexed → scoped per-repo health for the cwd repo
//! - No `--repo`, cwd not indexed → registry-level overview (same as `--repo @all`)
//! - `--repo <sel>` → per-repo health (frameworks / freshness / blind spots)
//!   for each resolved repo
//! - `--repo @all`  → registry-level overview (indexed repos + groups)
//!
//! `blind_spots` here lists only Type 1 (LLM-signal) buckets: source-code
//! opacity sites such as dynamic-import / reflection / eval / exec. Type 2
//! parser-metric buckets (uid-collision / method-overload / ifdef-redef) are
//! exposed via the hidden `ecp dev uid-audit` instead — keeping this surface
//! focused on what an LLM consumer can act on.
//!
//! External-client (HTTP/DB/Redis/queue) usage detail is intentionally NOT
//! folded here — that requires per-callsite binding analysis whose granularity
//! sits beyond a health summary. See the standalone `ecp tool-map` command.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::registry::{resolve_home_ecp, CommitBuildMeta, Registry, RegistryFile};
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct SummaryArgs {
    /// Repository selector (path | name | @all | csv mix).
    /// If omitted and cwd is an indexed repo: scoped per-repo health.
    /// If omitted and cwd is not indexed: registry overview.
    /// Use `--repo @all` to force the registry overview regardless of cwd.
    #[arg(long)]
    pub repo: Option<String>,

    /// Verbose per-section breakdown (include branch rows, etc.).
    #[arg(long, default_value_t = false)]
    pub detailed: bool,

    /// Output format. Omit for the LLM-tuned default (toon-encoded, lossy
    /// confidence rounding + compact timestamps). `--format toon` is the
    /// neutral toon encoding of the full payload; `--format json` is the
    /// full-fidelity JSON; `--format text` is the human-friendly fallback.
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: SummaryArgs, _graph_arg: &Path) -> Result<(), EcpError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let payload = build_payload(&args, _graph_arg)?;
    emit(&payload, format)
}

pub fn build_payload(args: &SummaryArgs, _graph_arg: &Path) -> Result<Value, EcpError> {
    let home_ecp = resolve_home_ecp();
    let registry = Registry::open(&home_ecp)
        .map_err(|e| EcpError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();

    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_str = cwd.to_string_lossy();

    let mut sections: serde_json::Map<String, Value> = serde_json::Map::new();

    match args.repo.as_deref() {
        // `--repo @all` → registry overview regardless of cwd.
        Some("@all") => {
            sections.insert(
                "indexed_repos".into(),
                build_registry_overview(reg, args.detailed),
            );
            sections.insert("groups".into(), build_groups_overview(reg));
        }
        // Explicit selector → per-repo health.
        Some(repo_sel) => {
            let selector = crate::repo_selector::parse(repo_sel)
                .map_err(|e| EcpError::InvalidArgument(format!("--repo selector: {e}")))?;
            let resolved = crate::repo_selector::resolve_top_level_indexing(
                &selector, reg, &cwd_str, "summary",
            )
            .map_err(|e| EcpError::InvalidArgument(format!("--repo: {e}")))?;
            let per_repo: Vec<Value> = resolved
                .iter()
                .map(|r| build_repo_health(r, args.detailed))
                .collect();
            sections.insert("per_repo".into(), Value::Array(per_repo));
        }
        // No `--repo`: scope to cwd repo when indexed; fall back to registry overview.
        None => {
            if let Some(alias) = crate::repo_selector::find_by_path(reg, &cwd_str) {
                let resolved = crate::repo_selector::ResolvedRepo {
                    dir_name: alias.dir_name.clone(),
                    common_dir: alias.common_dir.clone(),
                    aliases: alias.aliases.clone(),
                };
                sections.insert(
                    "per_repo".into(),
                    Value::Array(vec![build_repo_health(&resolved, args.detailed)]),
                );
            } else {
                sections.insert(
                    "indexed_repos".into(),
                    build_registry_overview(reg, args.detailed),
                );
                sections.insert("groups".into(), build_groups_overview(reg));
            }
        }
    }

    Ok(json!({ "summary": Value::Object(sections) }))
}

// ── Registry overview helpers ────────────────────────────────────────────────

fn build_registry_overview(reg: &RegistryFile, detailed: bool) -> Value {
    let rows: Vec<Value> = reg
        .repos
        .iter()
        .map(|(dir_name, alias)| {
            let display_name = alias
                .aliases
                .first()
                .map(|s| s.as_str())
                .unwrap_or(dir_name);
            let mut row = serde_json::Map::new();
            row.insert("name".into(), json!(display_name));
            // Omit dir_name when redundant (byte-identical to the display name).
            if display_name != dir_name.as_str() {
                row.insert("dir_name".into(), json!(dir_name));
            }
            row.insert("last_touched".into(), json!(alias.last_touched));
            // Omit groups when empty — carries no information.
            if !alias.groups.is_empty() {
                row.insert("groups".into(), json!(alias.groups));
            }
            Value::Object(row)
        })
        .collect();

    let _ = detailed; // detailed breakdown reserved for a future pass
    json!({ "count": rows.len(), "rows": rows })
}

fn build_groups_overview(reg: &RegistryFile) -> Value {
    let rows: Vec<Value> = reg
        .groups
        .iter()
        .map(|g| json!({ "name": g.name, "members": g.members.len() }))
        .collect();
    json!({ "count": rows.len(), "rows": rows })
}

// ── Per-repo health ──────────────────────────────────────────────────────────

pub fn build_repo_health(r: &crate::repo_selector::ResolvedRepo, detailed: bool) -> Value {
    // Load the graph once per repo and share it between framework + blind-spot
    // sections. Without this, each section would mmap+validate independently
    // — wasteful when `--repo @all` spans many repos.
    let engine = try_load_engine(r);
    let (graph, status) = match engine.as_ref() {
        None => (None, Some("graph_unavailable")),
        Some(e) => match e.graph() {
            Ok(g) => (Some(g), None),
            Err(_) => (None, Some("graph_load_failed")),
        },
    };
    let display_name = r.aliases.first().map(|s| s.as_str()).unwrap_or(&r.dir_name);
    json!({
        "repo": display_name,
        "dir_name": r.dir_name,
        "frameworks": fetch_frameworks(graph, status),
        "freshness": fetch_freshness(r, detailed),
        "metrics": fetch_metrics(graph, latest_graph_path(r).as_deref(), status),
        "blind_spots": fetch_blind_spots(graph, status),
    })
}

/// Find the most-recently-modified graph.bin under `<home_ecp>/<dir_name>/commits/`.
/// v2 is content-addressed per commit; we pick the newest one for coverage
/// reporting (the HEAD commit's build, if present).
fn latest_graph_path(r: &crate::repo_selector::ResolvedRepo) -> Option<PathBuf> {
    crate::commit_lookup::latest_graph_bin(&resolve_home_ecp(), &r.dir_name)
}

/// Open the repo's graph for read. Returns `None` for any failure — caller
/// degrades gracefully (emits zero counts + a status note) instead of failing
/// the whole `coverage` report when one repo's graph is missing or corrupt.
fn try_load_engine(r: &crate::repo_selector::ResolvedRepo) -> Option<Engine> {
    Engine::load(latest_graph_path(r)?).ok()
}

/// Freshness check: compare the latest graph.bin mtime to newest source file.
/// Uses `common_dir` as a proxy for the worktree root (parent of `.git`).
fn fetch_freshness(r: &crate::repo_selector::ResolvedRepo, detailed: bool) -> Value {
    use crate::auto_ensure::{ensure_index, EnsureResult};

    let Some(graph_path) = latest_graph_path(r) else {
        return json!({ "status": "missing" });
    };
    // Derive worktree root from common_dir (parent of `.git`).
    // Via the shared helper so a Windows verbatim `\\?\` prefix in the stored
    // common_dir is stripped before it reaches `ensure_index`'s WalkBuilder
    // (else os error 267 on Windows — see git_cache::worktree_root_from_common_dir).
    let common = Path::new(&r.common_dir);
    let worktree = crate::git_cache::worktree_root_from_common_dir(common);

    let mut out = match ensure_index(&graph_path, worktree) {
        Ok(EnsureResult::Ready) => json!({ "status": "ready" }),
        Ok(EnsureResult::Stale { age_seconds, .. }) => {
            json!({ "status": "stale", "age_seconds": age_seconds })
        }
        Ok(EnsureResult::Missing) => json!({ "status": "missing" }),
        Err(e) => json!({ "status": "error", "error": e.to_string() }),
    };

    let Some(map) = out.as_object_mut() else {
        return out;
    };

    map.insert(
        "current_head_short".into(),
        match crate::git::safe_exec::head_short(worktree) {
            Some(sha) => json!(sha),
            None => Value::Null,
        },
    );

    // Surface graph-builder vs current-binary SHA mismatch (FU-005). Helps
    // detect "the binary that built this graph is older than the running ecp"
    // — common cause of unexplained stale behavior after a `cargo install`.
    let graph_builder_sha: Option<String> = graph_path
        .parent()
        .map(|d| d.join("meta.json"))
        .and_then(|p| CommitBuildMeta::read(&p).ok())
        .and_then(|m| m.binary_commit_sha);
    let current_binary_sha = env!("ECP_GIT_SHA");
    map.insert(
        "graph_builder_sha".into(),
        graph_builder_sha
            .as_deref()
            .map(|s| json!(s))
            .unwrap_or(Value::Null),
    );
    map.insert("current_binary_sha".into(), json!(current_binary_sha));
    if let Some(g) = graph_builder_sha.as_deref() {
        if g != current_binary_sha {
            eprintln!(
                "warn: graph built by ecp@{} differs from current binary ecp@{}; run `ecp admin index --force` if results look stale",
                g, current_binary_sha
            );
        }
    }

    let _ = detailed;
    out
}

/// Post-index metrics surfaced inline so an LLM doesn't have to run a
/// follow-up Cypher to learn "how big is this graph". Matches the gitnexus
/// precedent of shipping a quantitative summary right after indexing.
///
/// - `nodes` / `edges` / `files`: totals (nodes includes Process / synthetic)
/// - `symbols`: callable / type-bearing nodes (Function, Method, Class,
///   Interface) — the things the LLM is most likely to ask about
/// - `by_kind`: per-NodeKind counts (only non-zero kinds), so "how many
///   functions / classes / routes" needs no follow-up query
/// - `graph_version`: rkyv `graph.bin` format version
/// - `graph_bytes`: on-disk size of the graph.bin backing this repo
fn fetch_metrics(
    graph: Option<&ArchivedZeroCopyGraph>,
    graph_path: Option<&std::path::Path>,
    status: Option<&'static str>,
) -> Value {
    match graph {
        Some(g) => {
            let nodes = g.nodes.len();
            let edges = g.edges.len();
            let files = g.files.len();
            // Single walk: total symbols + per-kind tally. NodeKind derives
            // `#[rkyv(compare(PartialEq))]`, so the archived value compares
            // against the owned enum without a per-node deserialize.
            use ecp_core::graph::NodeKind;
            let mut symbols: u32 = 0;
            let mut by_kind: std::collections::BTreeMap<&'static str, u32> =
                std::collections::BTreeMap::new();
            for node in g.nodes.iter() {
                let kind = NodeKind::from(&node.kind);
                *by_kind.entry(kind.as_str()).or_insert(0) += 1;
                if matches!(
                    kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Interface
                ) {
                    symbols += 1;
                }
            }
            json!({
                "nodes": nodes,
                "edges": edges,
                "files": files,
                "symbols": symbols,
                "by_kind": by_kind,
                "graph_version": g.version.to_native(),
                "graph_bytes": graph_path.and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()),
            })
        }
        None => json!({
            "nodes": 0,
            "edges": 0,
            "files": 0,
            "symbols": 0,
            "status": status.unwrap_or("graph_unavailable"),
        }),
    }
}

/// Framework coverage: the static supported catalog plus a `detected` list
/// derived from edge `reason` tags in the live graph. When the graph is
/// missing or unreadable, `detected` is `[]` and `status` explains.
fn fetch_frameworks(graph: Option<&ArchivedZeroCopyGraph>, status: Option<&'static str>) -> Value {
    let supported = supported_framework_catalog();
    let detected = graph
        .map(count_detected_frameworks)
        .unwrap_or_else(|| json!([]));

    let mut out = serde_json::Map::new();
    out.insert("supported_count".into(), json!(supported.len()));
    out.insert("supported".into(), Value::Array(supported));
    out.insert("detected".into(), detected);
    if let Some(s) = status {
        out.insert("status".into(), json!(s));
    }
    Value::Object(out)
}

/// The supported-frameworks catalog: static list of (lang_framework, reason_tag,
/// confidence) tuples returned alongside `detected` so downstream tooling can
/// identify frameworks the analyzer supports but hasn't seen in this graph.
fn supported_framework_catalog() -> Vec<Value> {
    use ecp_analyzer::framework_confidence as fc;

    let patterns: &[(&str, &str)] = &[
        ("Python/FastAPI", "fastapi-depends"),
        ("Python/FastAPI", "fastapi-route-<method>"),
        ("Python/Django", "django-url-path"),
        ("Python/Django", "django-signal-receiver"),
        ("Python/Django", "django-signal-connect"),
        ("Python/Celery", "celery-task"),
        ("Python/reflection", "reflection-getattr-fanout"),
        ("Rust/Axum", "axum-route-handler"),
        ("Rust/Actix", "actix-route-<method>"),
        ("Web/Express", "express-route-handler"),
        ("TypeScript/NestJS", "nestjs-route-handler"),
        ("Java/Spring", "spring-autowired"),
        ("Java/Spring", "spring-route-handler"),
    ];

    let confidence_for = |tag: &str| -> f32 {
        match tag {
            "fastapi-depends" => fc::FASTAPI_DEPENDS,
            "fastapi-route-<method>" => fc::FASTAPI_ROUTE,
            "django-url-path" => fc::DJANGO_URL,
            "django-signal-receiver" | "django-signal-connect" => fc::DJANGO_SIGNAL,
            "celery-task" => fc::CELERY_TASK,
            "reflection-getattr-fanout" => fc::FANOUT_BASE,
            "axum-route-handler" => fc::AXUM_ROUTE,
            "actix-route-<method>" => fc::ACTIX_ROUTE,
            "express-route-handler" => fc::EXPRESS_ROUTE,
            "nestjs-route-handler" => fc::NESTJS_ROUTE,
            "spring-autowired" => fc::SPRING_AUTOWIRED,
            "spring-route-handler" => fc::SPRING_ROUTE,
            _ => 0.0,
        }
    };

    patterns
        .iter()
        .map(|(lang_fw, tag)| {
            json!({
                "lang_framework": lang_fw,
                "reason_tag": tag,
                "confidence": confidence_for(tag),
            })
        })
        .collect()
}

/// Map an edge `reason` string to the lang_framework it represents. Some
/// reasons carry a dynamic suffix (`fastapi-route-GET`, `actix-route-POST`)
/// — match those by prefix. The `Web/Express` bucket covers both the JS
/// parser tag (`"express-route"`) and the TS parser tag
/// (`"express-route-handler"`). Returns `None` for non-framework reasons.
fn classify_framework_reason(reason: &str) -> Option<&'static str> {
    if reason == "fastapi-depends" || reason.starts_with("fastapi-route-") {
        Some("Python/FastAPI")
    } else if reason == "django-url-path" || reason.starts_with("django-signal-") {
        Some("Python/Django")
    } else if reason == "celery-task" {
        Some("Python/Celery")
    } else if reason == "reflection-getattr-fanout" {
        Some("Python/reflection")
    } else if reason == "axum-route-handler" {
        Some("Rust/Axum")
    } else if reason.starts_with("actix-route-") {
        Some("Rust/Actix")
    } else if reason == "express-route" || reason == "express-route-handler" {
        Some("Web/Express")
    } else if reason == "nestjs-route-handler" {
        Some("TypeScript/NestJS")
    } else if reason == "spring-autowired" || reason == "spring-route-handler" {
        Some("Java/Spring")
    } else {
        None
    }
}

/// Walk graph edges, group framework-attributable reason tags by
/// `lang_framework`, return one row per framework with edge count.
/// `BTreeMap` keeps output stable for snapshot-style assertions.
fn count_detected_frameworks(graph: &ArchivedZeroCopyGraph) -> Value {
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    for edge in graph.edges.iter() {
        let reason = edge.reason.resolve(&graph.string_pool);
        if let Some(fw) = classify_framework_reason(reason) {
            *counts.entry(fw).or_insert(0) += 1;
        }
    }
    json!(counts
        .into_iter()
        .map(|(fw, count)| json!({ "lang_framework": fw, "edge_count": count }))
        .collect::<Vec<_>>())
}

/// Blind spots: unsupported dynamic-dispatch sites recorded by the analyzer
/// during indexing. Read directly from `graph.blind_spots` (no extra
/// scanning). Falls back to a `status` note when the graph is unavailable.
fn fetch_blind_spots(graph: Option<&ArchivedZeroCopyGraph>, status: Option<&'static str>) -> Value {
    match graph {
        Some(g) => count_blind_spots(g),
        None => {
            json!({ "total": 0, "by_kind": {}, "status": status.unwrap_or("graph_unavailable") })
        }
    }
}

/// Group `graph.blind_spots` by their `kind` tag (e.g. `dynamic-import`,
/// `reflection`), excluding Type 2 parser-metric buckets — those are for
/// parser maintainers, not LLM consumers, and live under `ecp dev uid-audit`.
/// The filter set is owned by [`ecp_core::graph::is_dev_metric_bs_kind`] so
/// `ecp dev uid-audit` and any future Type-2-aware consumer agree on which
/// kinds are dev-metric without re-typing the literals here.
/// Keys borrow zero-copy from `graph.string_pool`; the `BTreeMap` makes the
/// output deterministic for snapshot-style assertions.
fn count_blind_spots(graph: &ArchivedZeroCopyGraph) -> Value {
    let mut by_kind: BTreeMap<&str, u32> = BTreeMap::new();
    for bs in graph.blind_spots.iter() {
        let kind = bs.kind.resolve(&graph.string_pool);
        if ecp_core::graph::is_dev_metric_bs_kind(kind) {
            continue;
        }
        *by_kind.entry(kind).or_insert(0) += 1;
    }
    let total: u32 = by_kind.values().sum();
    json!({ "total": total, "by_kind": by_kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{NodeKind, RelType, ZeroCopyGraph};
    use ecp_core::graph_fixture::GraphFixture;

    /// rkyv-archive an in-memory `ZeroCopyGraph` and pass the borrowed
    /// `ArchivedZeroCopyGraph` into the test body.
    fn with_archived(g: ZeroCopyGraph, f: impl FnOnce(&ArchivedZeroCopyGraph)) {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec();
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        f(archived);
    }

    #[test]
    fn count_blind_spots_empty_returns_zero_total() {
        let g = GraphFixture::new().build();
        with_archived(g, |archived| {
            let v = count_blind_spots(archived);
            assert_eq!(v["total"], json!(0));
            assert!(v["by_kind"].as_object().unwrap().is_empty());
        });
    }

    /// `summary.blind_spots` is the LLM-facing surface; Type 2 parser-metric
    /// kinds (uid-collision / method-overload / ifdef-redef) must be excluded
    /// so the surface stays focused on source-code opacity an LLM can act on.
    /// Inspecting Type 2 buckets is `ecp dev uid-audit`'s job.
    #[test]
    fn count_blind_spots_excludes_dev_metric_kinds() {
        let mut fx = GraphFixture::new();
        for kind in [
            "dynamic-import",
            "uid-collision",
            "method-overload",
            "ifdef-redef",
        ] {
            fx.blind_spot(kind, "src/x.py", (1, 0, 1, 10), "");
        }
        let g = fx.build();

        with_archived(g, |archived| {
            let v = count_blind_spots(archived);
            // Only Type 1 (`dynamic-import`) survives the filter; the three
            // Type 2 kinds are stripped.
            assert_eq!(v["total"], json!(1));
            assert_eq!(v["by_kind"]["dynamic-import"], json!(1));
            assert!(v["by_kind"].get("uid-collision").is_none());
            assert!(v["by_kind"].get("method-overload").is_none());
            assert!(v["by_kind"].get("ifdef-redef").is_none());
        });
    }

    #[test]
    fn count_blind_spots_groups_by_kind() {
        let mut fx = GraphFixture::new();
        fx.blind_spot("dynamic-import", "src/x.py", (1, 0, 1, 10), "");
        fx.blind_spot("dynamic-import", "src/x.py", (2, 0, 2, 10), "");
        fx.blind_spot("reflection", "src/x.py", (3, 0, 3, 10), "");
        let g = fx.build();

        with_archived(g, |archived| {
            let v = count_blind_spots(archived);
            assert_eq!(v["total"], json!(3));
            assert_eq!(v["by_kind"]["dynamic-import"], json!(2));
            assert_eq!(v["by_kind"]["reflection"], json!(1));
        });
    }

    #[test]
    fn classify_framework_reason_known_tags() {
        assert_eq!(
            classify_framework_reason("fastapi-depends"),
            Some("Python/FastAPI")
        );
        assert_eq!(
            classify_framework_reason("fastapi-route-GET"),
            Some("Python/FastAPI")
        );
        assert_eq!(
            classify_framework_reason("django-url-path"),
            Some("Python/Django")
        );
        assert_eq!(
            classify_framework_reason("django-signal-receiver"),
            Some("Python/Django")
        );
        assert_eq!(
            classify_framework_reason("django-signal-connect"),
            Some("Python/Django")
        );
        assert_eq!(
            classify_framework_reason("celery-task"),
            Some("Python/Celery")
        );
        assert_eq!(
            classify_framework_reason("reflection-getattr-fanout"),
            Some("Python/reflection")
        );
        assert_eq!(
            classify_framework_reason("axum-route-handler"),
            Some("Rust/Axum")
        );
        assert_eq!(
            classify_framework_reason("actix-route-POST"),
            Some("Rust/Actix")
        );
        assert_eq!(
            classify_framework_reason("express-route-handler"),
            Some("Web/Express")
        );
        // JS parser emits the shorter tag; both must route to the same bucket
        // so JS-only Express apps aren't silently dropped from `detected`.
        assert_eq!(
            classify_framework_reason("express-route"),
            Some("Web/Express")
        );
        assert_eq!(
            classify_framework_reason("nestjs-route-handler"),
            Some("TypeScript/NestJS")
        );
        assert_eq!(
            classify_framework_reason("spring-autowired"),
            Some("Java/Spring")
        );
        assert_eq!(
            classify_framework_reason("spring-route-handler"),
            Some("Java/Spring")
        );
    }

    #[test]
    fn classify_framework_reason_unknown_returns_none() {
        assert_eq!(classify_framework_reason("ast-call"), None);
        assert_eq!(classify_framework_reason(""), None);
        assert_eq!(classify_framework_reason("calls"), None);
        assert_eq!(classify_framework_reason("django-other"), None);
    }

    #[test]
    fn count_detected_frameworks_groups_edges_by_framework() {
        let mut fx = GraphFixture::new();
        let a = fx.func("src/x.py", "a");
        fx.span(a, (0, 0, 1, 0));
        let b = fx.func("src/x.py", "b");
        fx.span(b, (1, 0, 2, 0));
        fx.edge_with(a, b, RelType::Calls, 1.0, "fastapi-depends");
        fx.edge_with(a, b, RelType::Calls, 1.0, "fastapi-route-GET");
        fx.edge_with(a, b, RelType::Calls, 1.0, "axum-route-handler");
        fx.edge_with(a, b, RelType::Calls, 1.0, "ast-call");
        let g = fx.build();

        with_archived(g, |archived| {
            let v = count_detected_frameworks(archived);
            let arr = v.as_array().expect("detected is array");
            // BTreeMap keys: alphabetical → "Python/FastAPI", "Rust/Axum"
            assert_eq!(arr.len(), 2);
            assert_eq!(arr[0]["lang_framework"], json!("Python/FastAPI"));
            assert_eq!(arr[0]["edge_count"], json!(2));
            assert_eq!(arr[1]["lang_framework"], json!("Rust/Axum"));
            assert_eq!(arr[1]["edge_count"], json!(1));
        });
    }

    #[test]
    fn count_detected_frameworks_empty_graph_returns_empty_array() {
        let g = GraphFixture::new().build();
        with_archived(g, |archived| {
            let v = count_detected_frameworks(archived);
            assert_eq!(v, json!([]));
        });
    }

    #[test]
    fn fetch_metrics_counts_nodes_edges_files_and_symbols() {
        let mut fx = GraphFixture::new();
        // Three nodes: one symbol-eligible (Function), one symbol-eligible
        // (Class), one ineligible (Variable). Expect symbols = 2.
        let f = fx.func("src/x.py", "f");
        fx.span(f, (0, 0, 1, 0));
        let c = fx.node(NodeKind::Class, "src/x.py", "C");
        fx.span(c, (2, 0, 3, 0));
        let var = fx.node(NodeKind::Variable, "src/x.py", "v");
        fx.span(var, (4, 0, 5, 0));
        fx.edge(f, c, RelType::Calls);
        let g = fx.build();

        with_archived(g, |archived| {
            let v = fetch_metrics(Some(archived), None, None);
            assert_eq!(v["nodes"], json!(3));
            assert_eq!(v["edges"], json!(1));
            assert_eq!(v["files"], json!(1));
            assert_eq!(v["symbols"], json!(2));
            // by_kind tallies every node kind present (Function + Class + Variable).
            assert_eq!(v["by_kind"]["Function"], json!(1));
            assert_eq!(v["by_kind"]["Class"], json!(1));
            assert_eq!(v["by_kind"]["Variable"], json!(1));
            // graph_bytes is null when no path supplied; version is surfaced.
            assert!(v["graph_bytes"].is_null());
            assert!(v["graph_version"].is_number());
            assert!(v.get("status").is_none());
        });
    }

    #[test]
    fn fetch_metrics_no_graph_returns_zeros_with_status_note() {
        let v = fetch_metrics(None, None, Some("graph_unavailable"));
        assert_eq!(v["nodes"], json!(0));
        assert_eq!(v["edges"], json!(0));
        assert_eq!(v["files"], json!(0));
        assert_eq!(v["symbols"], json!(0));
        assert_eq!(v["status"], json!("graph_unavailable"));
    }

    #[test]
    fn fetch_freshness_returns_status_for_missing_graph() {
        use crate::repo_selector::ResolvedRepo;
        // common_dir points nowhere → no graph → status: missing
        let r = ResolvedRepo {
            dir_name: "demo__aabbccdd".into(),
            common_dir: "/nope/not-a-real-path/.git".into(),
            aliases: vec!["demo".into()],
        };
        let v = fetch_freshness(&r, false);
        // graph_path will be None (no commits dir) → status: missing
        assert!(v.get("status").is_some());
    }

    #[test]
    fn build_repo_health_payload_has_metrics_alongside_freshness() {
        // Locks the per_repo payload contract that LLM consumers depend on:
        // {repo, frameworks, freshness, metrics, blind_spots}. The path
        // points nowhere, so the graph load fails and we hit the
        // graph_unavailable status; we still assert the keys exist (with
        // zeros) so a future refactor that drops `metrics` or renames a
        // section fails the test instead of silently breaking the contract.
        use crate::repo_selector::ResolvedRepo;
        let r = ResolvedRepo {
            dir_name: "demo__aabbccdd".into(),
            common_dir: "/nope/not-a-real-path/.git".into(),
            aliases: vec!["demo".into()],
        };
        let v = build_repo_health(&r, true);
        assert_eq!(v["repo"], json!("demo"));
        assert!(v.get("frameworks").is_some(), "frameworks section missing");
        assert!(v.get("freshness").is_some(), "freshness section missing");
        assert!(v.get("metrics").is_some(), "metrics section missing");
        assert!(
            v.get("blind_spots").is_some(),
            "blind_spots section missing"
        );
        let metrics = &v["metrics"];
        assert_eq!(metrics["nodes"], json!(0));
        assert_eq!(metrics["edges"], json!(0));
        assert_eq!(metrics["files"], json!(0));
        assert_eq!(metrics["symbols"], json!(0));
        assert_eq!(metrics["status"], json!("graph_unavailable"));
    }
}
