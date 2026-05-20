//! `ecp coverage` — unified registry + repo health entry point.
//!
//! Folds doctor + status + list + summarize into one command:
//!
//! - No `--repo`     → registry-level overview (indexed repos + groups)
//! - `--repo <sel>`  → per-repo health (frameworks / freshness / blind spots)
//!   for each resolved repo
//! - `--repo @group` → same, aggregated for all group members
//!
//! External-client (HTTP/DB/Redis/queue) usage detail is intentionally NOT
//! folded here — that requires per-callsite binding analysis whose granularity
//! sits beyond a health summary. See the standalone `ecp tool-map` command.

use crate::commit_lookup::find_latest_by_mtime;
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::registry::{resolve_home_ecp, Registry, RegistryFile};
use ecp_core::EcpError;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, Clone)]
pub struct CoverageArgs {
    /// Repository selector (path | name | @group | @all | csv mix).
    /// If omitted: registry-level overview only.
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

pub fn run(args: CoverageArgs, _graph_arg: &Path) -> Result<(), EcpError> {
    let format = OutputFormat::parse(args.format.as_deref());
    let payload = build_payload(&args, _graph_arg)?;
    emit(&payload, format)
}

pub fn build_payload(args: &CoverageArgs, _graph_arg: &Path) -> Result<Value, EcpError> {
    let home_ecp = resolve_home_ecp();
    let registry = Registry::open(&home_ecp)
        .map_err(|e| EcpError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();

    let cwd = std::env::current_dir().unwrap_or_default();

    let mut sections: serde_json::Map<String, Value> = serde_json::Map::new();

    // `--repo` acts as filter: drop the registry-wide overview to keep
    // single-repo output focused (the per_repo section already contains
    // the relevant entries). Without `--repo`, fall back to registry view.
    if let Some(repo_sel) = args.repo.as_deref() {
        let selector = crate::repo_selector::parse(repo_sel)
            .map_err(|e| EcpError::Output(format!("selector: {e}")))?;
        let cwd_str = cwd.to_string_lossy();
        let resolved =
            crate::repo_selector::resolve_top_level(&selector, reg, &cwd_str, "coverage")
                .map_err(|e| EcpError::Output(format!("selector: {e}")))?;
        let per_repo: Vec<Value> = resolved
            .iter()
            .map(|r| build_repo_health(r, args.detailed))
            .collect();
        sections.insert("per_repo".into(), Value::Array(per_repo));
    } else {
        sections.insert(
            "indexed_repos".into(),
            build_registry_overview(reg, args.detailed),
        );
        sections.insert("groups".into(), build_groups_overview(reg));
    }

    Ok(json!({ "coverage": Value::Object(sections) }))
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
            json!({
                "name": display_name,
                "dir_name": dir_name,
                "last_touched": alias.last_touched,
                "groups": alias.groups,
            })
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
        "metrics": fetch_metrics(graph, status),
        "blind_spots": fetch_blind_spots(graph, status),
    })
}

/// Find the most-recently-modified graph.bin under `<home_ecp>/<dir_name>/commits/`.
/// v2 is content-addressed per commit; we pick the newest one for coverage
/// reporting (the HEAD commit's build, if present). Delegates to
/// `commit_lookup::find_latest_by_mtime` (handles `.building` / `.stale-*`
/// filtering); we append `graph.bin` since that helper returns the commit dir.
fn latest_graph_path(r: &crate::repo_selector::ResolvedRepo) -> Option<PathBuf> {
    let commits_dir = resolve_home_ecp().join(&r.dir_name).join("commits");
    find_latest_by_mtime(&commits_dir).map(|dir| dir.join("graph.bin"))
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
    let common = Path::new(&r.common_dir);
    let worktree = common.parent().unwrap_or(common);

    let mut out = match ensure_index(&graph_path, worktree) {
        Ok(EnsureResult::Ready) => json!({ "status": "ready" }),
        Ok(EnsureResult::Stale { age_seconds }) => {
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

    let _ = detailed;
    out
}

/// Four post-index metrics surfaced inline so an LLM doesn't have to run a
/// follow-up Cypher to learn "how big is this graph". Matches the gitnexus
/// precedent of shipping a quantitative summary right after indexing.
///
/// - `nodes`: total node count (includes Process / synthetic nodes)
/// - `edges`: total edge count
/// - `files`: distinct source files indexed
/// - `symbols`: callable / type-bearing nodes (Function, Method, Class,
///   Interface) — the things the LLM is most likely to ask about
fn fetch_metrics(graph: Option<&ArchivedZeroCopyGraph>, status: Option<&'static str>) -> Value {
    match graph {
        Some(g) => {
            let nodes = g.nodes.len();
            let edges = g.edges.len();
            let files = g.files.len();
            // NodeKind derives `#[rkyv(compare(PartialEq))]`, so the archived
            // value can be compared against the owned enum directly — no
            // per-node deserialize. Skipping the deserialize avoids a heap
            // alloc per node for large graphs (`--repo @all` × ~10k nodes).
            use ecp_core::graph::NodeKind;
            let mut symbols: u32 = 0;
            for node in g.nodes.iter() {
                if node.kind == NodeKind::Function
                    || node.kind == NodeKind::Method
                    || node.kind == NodeKind::Class
                    || node.kind == NodeKind::Interface
                {
                    symbols += 1;
                }
            }
            json!({ "nodes": nodes, "edges": edges, "files": files, "symbols": symbols })
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
/// `reflection`). Keys borrow zero-copy from `graph.string_pool`; the
/// `BTreeMap` makes the output deterministic for snapshot-style assertions.
fn count_blind_spots(graph: &ArchivedZeroCopyGraph) -> Value {
    let mut by_kind: BTreeMap<&str, u32> = BTreeMap::new();
    for bs in graph.blind_spots.iter() {
        let kind = bs.kind.resolve(&graph.string_pool);
        *by_kind.entry(kind).or_insert(0) += 1;
    }
    let total: u32 = by_kind.values().sum();
    json!({ "total": total, "by_kind": by_kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecp_core::graph::{
        BlindSpotRecord, Edge, File, FileCategory, Node, NodeKind, RelType, ZeroCopyGraph,
        GRAPH_FORMAT_VERSION, GRAPH_MAGIC,
    };
    use ecp_core::pool::StringPool;

    /// rkyv-archive an in-memory `ZeroCopyGraph` and pass the borrowed
    /// `ArchivedZeroCopyGraph` into the test body.
    fn with_archived(g: ZeroCopyGraph, f: impl FnOnce(&ArchivedZeroCopyGraph)) {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&g).unwrap().to_vec();
        let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
        f(archived);
    }

    fn empty_graph(pool: StringPool) -> ZeroCopyGraph {
        ZeroCopyGraph {
            magic: GRAPH_MAGIC,
            version: GRAPH_FORMAT_VERSION,
            fingerprint: [0; 32],
            string_pool: pool.bytes,
            files: vec![],
            nodes: vec![],
            edges: vec![],
            out_offsets: vec![0],
            in_offsets: vec![0],
            in_edge_idx: vec![],
            name_index: vec![],
            process_start: 0,
            traces_offsets: vec![],
            traces_data: vec![],
            blind_spots: vec![],
            route_shapes: vec![],
            call_metas: vec![],
            function_metas: vec![],
        }
    }

    #[test]
    fn count_blind_spots_empty_returns_zero_total() {
        let pool = StringPool::new();
        let g = empty_graph(pool);
        with_archived(g, |archived| {
            let v = count_blind_spots(archived);
            assert_eq!(v["total"], json!(0));
            assert!(v["by_kind"].as_object().unwrap().is_empty());
        });
    }

    #[test]
    fn count_blind_spots_groups_by_kind() {
        let mut pool = StringPool::new();
        let kind_dyn = pool.add("dynamic-import");
        let kind_refl = pool.add("reflection");
        let path = pool.add("src/x.py");
        let hint = pool.add("");
        let bs1 = BlindSpotRecord {
            kind: kind_dyn,
            file_path: path,
            start_row: 1,
            start_col: 0,
            end_row: 1,
            end_col: 10,
            hint,
        };
        let bs2 = BlindSpotRecord {
            kind: kind_dyn,
            file_path: path,
            start_row: 2,
            start_col: 0,
            end_row: 2,
            end_col: 10,
            hint,
        };
        let bs3 = BlindSpotRecord {
            kind: kind_refl,
            file_path: path,
            start_row: 3,
            start_col: 0,
            end_row: 3,
            end_col: 10,
            hint,
        };
        let mut g = empty_graph(pool);
        g.blind_spots = vec![bs1, bs2, bs3];

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
        let mut pool = StringPool::new();
        let name_a = pool.add("a");
        let name_b = pool.add("b");
        let path = pool.add("src/x.py");
        let uid_a = pool.add("0:a");
        let uid_b = pool.add("0:b");
        let r_fastapi_dep = pool.add("fastapi-depends");
        let r_fastapi_route = pool.add("fastapi-route-GET");
        let r_axum = pool.add("axum-route-handler");
        let r_unknown = pool.add("ast-call");

        let mut g = empty_graph(pool);
        g.files = vec![File {
            path,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }];
        g.nodes = vec![
            Node {
                uid: uid_a,
                name: name_a,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (0, 0, 1, 0),
                community_id: 0,
            },
            Node {
                uid: uid_b,
                name: name_b,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (1, 0, 2, 0),
                community_id: 0,
            },
        ];
        g.edges = vec![
            Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason: r_fastapi_dep,
            },
            Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason: r_fastapi_route,
            },
            Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason: r_axum,
            },
            Edge {
                source: 0,
                target: 1,
                rel_type: RelType::Calls,
                confidence: 1.0,
                reason: r_unknown,
            },
        ];
        g.out_offsets = vec![0, 4, 4];
        g.in_offsets = vec![0, 0, 4];
        g.in_edge_idx = vec![0, 1, 2, 3];
        g.process_start = 2;

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
        let pool = StringPool::new();
        let g = empty_graph(pool);
        with_archived(g, |archived| {
            let v = count_detected_frameworks(archived);
            assert_eq!(v, json!([]));
        });
    }

    #[test]
    fn fetch_metrics_counts_nodes_edges_files_and_symbols() {
        let mut pool = StringPool::new();
        let name_f = pool.add("f");
        let name_c = pool.add("C");
        let name_v = pool.add("v");
        let path = pool.add("src/x.py");
        let uid_f = pool.add("0:f");
        let uid_c = pool.add("0:C");
        let uid_v = pool.add("0:v");

        let mut g = empty_graph(pool);
        g.files = vec![File {
            path,
            mtime: 0,
            content_hash: [0; 8],
            category: FileCategory::Source,
        }];
        // Three nodes: one symbol-eligible (Function), one symbol-eligible
        // (Class), one ineligible (Variable). Expect symbols = 2.
        g.nodes = vec![
            Node {
                uid: uid_f,
                name: name_f,
                file_idx: 0,
                kind: NodeKind::Function,
                span: (0, 0, 1, 0),
                community_id: 0,
            },
            Node {
                uid: uid_c,
                name: name_c,
                file_idx: 0,
                kind: NodeKind::Class,
                span: (2, 0, 3, 0),
                community_id: 0,
            },
            Node {
                uid: uid_v,
                name: name_v,
                file_idx: 0,
                kind: NodeKind::Variable,
                span: (4, 0, 5, 0),
                community_id: 0,
            },
        ];
        g.edges = vec![Edge {
            source: 0,
            target: 1,
            rel_type: RelType::Calls,
            confidence: 1.0,
            reason: name_f,
        }];
        g.out_offsets = vec![0, 1, 1, 1];
        g.in_offsets = vec![0, 0, 1, 1];
        g.in_edge_idx = vec![0];
        g.process_start = 3;

        with_archived(g, |archived| {
            let v = fetch_metrics(Some(archived), None);
            assert_eq!(v["nodes"], json!(3));
            assert_eq!(v["edges"], json!(1));
            assert_eq!(v["files"], json!(1));
            assert_eq!(v["symbols"], json!(2));
            assert!(v.get("status").is_none());
        });
    }

    #[test]
    fn fetch_metrics_no_graph_returns_zeros_with_status_note() {
        let v = fetch_metrics(None, Some("graph_unavailable"));
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
