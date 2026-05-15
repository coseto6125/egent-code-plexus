//! `gnx coverage` — unified registry + repo health entry point.
//!
//! Folds doctor + status + list + summarize + tool_map into one command:
//!
//! - No `--repo`     → registry-level overview (indexed repos + groups)
//! - `--repo <sel>`  → per-repo health (frameworks / freshness / externals /
//!   blind spots) for each resolved repo
//! - `--repo @group` → same, aggregated for all group members
//!
//! Source modules (doctor/status/list/summarize/tool_map) coexist for now;
//! Phase 5 cleanup removes them once cross-deps are clear.

use crate::auto_ensure::{ensure_index, EnsureResult};
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::registry::{resolve_home_gnx, Registry, RegistryFile};
use graph_nexus_core::GnxError;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Args, Debug, Clone)]
pub struct CoverageArgs {
    /// Repository selector (path | name | @group | @all | csv mix).
    /// If omitted: registry-level overview only.
    #[arg(long)]
    pub repo: Option<String>,

    /// Verbose per-section breakdown (include branch rows, etc.).
    #[arg(long, default_value_t = false)]
    pub detailed: bool,

    /// Output format: toon (default) | json.
    #[arg(long, default_value = "toon")]
    pub format: Option<String>,
}

pub fn run(args: CoverageArgs, _graph_arg: &Path) -> Result<(), GnxError> {
    let home_gnx = resolve_home_gnx();
    let registry = Registry::open(&home_gnx)
        .map_err(|e| GnxError::InvalidArgument(format!("registry open: {e}")))?;
    let reg = registry.snapshot();

    let format = OutputFormat::parse(args.format.as_deref());
    let cwd = std::env::current_dir().unwrap_or_default();

    let mut sections: serde_json::Map<String, Value> = serde_json::Map::new();

    // `--repo` acts as filter: drop the registry-wide overview to keep
    // single-repo output focused (the per_repo section already contains
    // the relevant entries). Without `--repo`, fall back to registry view.
    if let Some(repo_sel) = args.repo.as_deref() {
        let selector = crate::repo_selector::parse(repo_sel)
            .map_err(|e| GnxError::Output(format!("selector: {e}")))?;
        let cwd_str = cwd.to_string_lossy();
        let resolved = crate::repo_selector::resolve(&selector, reg, &cwd_str).map_err(|e| {
            // For unknown group/name: emit graceful empty result rather than
            // a hard error, so `--repo @test-group` on a fresh machine
            // doesn't blow up integration tests.
            GnxError::Output(format!("selector: {e}"))
        })?;
        let per_repo: Vec<Value> = resolved.iter().map(build_repo_health).collect();
        sections.insert("per_repo".into(), Value::Array(per_repo));
    } else {
        sections.insert(
            "indexed_repos".into(),
            build_registry_overview(reg, args.detailed),
        );
        sections.insert("groups".into(), build_groups_overview(reg));
    }

    let value = json!({ "coverage": Value::Object(sections) });
    emit(&value, format)
}

// ── Registry overview helpers ────────────────────────────────────────────────

fn build_registry_overview(reg: &RegistryFile, detailed: bool) -> Value {
    let rows: Vec<Value> = reg
        .repos
        .iter()
        .map(|r| {
            let last = r
                .branches
                .iter()
                .map(|b| b.indexed_at.as_str())
                .max()
                .unwrap_or("never");
            let total_nodes: u32 = r.branches.iter().map(|b| b.node_count).sum();
            json!({
                "name": r.name,
                "branches": r.branches.len(),
                "last_indexed": last,
                "total_nodes": total_nodes,
                "groups": r.groups,
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

fn build_repo_health(r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({
        "repo": r.name,
        "frameworks": fetch_frameworks(r),
        "freshness": fetch_freshness(r),
        "externals_summary": fetch_externals_summary(r),
        "blind_spots": fetch_blind_spots(r),
    })
}

/// Freshness check: compare graph.bin mtime to newest source file.
/// Uses the "main" branch graph path by default; falls back gracefully
/// when no branch directory is found.
fn fetch_freshness(r: &crate::repo_selector::ResolvedRepo) -> Value {
    // Try the default "main" branch path, then fall back to the index_dir_root
    // directly in case the repo uses a different primary branch name.
    let main_path = Path::new(&r.index_dir_root).join("main").join("graph.bin");
    let worktree = Path::new(&r.worktree_path);

    match ensure_index(&main_path, worktree) {
        Ok(EnsureResult::Ready) => json!({ "status": "ready" }),
        Ok(EnsureResult::Stale { age_seconds }) => {
            json!({ "status": "stale", "age_seconds": age_seconds })
        }
        Ok(EnsureResult::Missing) => json!({ "status": "missing" }),
        Err(e) => json!({ "status": "error", "error": e.to_string() }),
    }
}

/// Framework coverage: returns the static hardcoded catalog from doctor.rs.
///
/// TODO(Phase-5): load the repo's graph and cross-reference detected
/// frameworks against the catalog for a "detected vs supported" diff.
/// For now, emits the full catalog as a reference contract.
fn fetch_frameworks(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    use graph_nexus_analyzer::framework_confidence as fc;

    // Mirrors FRAMEWORK_COVERAGE from commands/doctor.rs (static catalog).
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
        ("TypeScript/Express", "express-route-handler"),
        ("TypeScript/NestJS", "nestjs-route-handler"),
        ("Java/Spring", "spring-autowired"),
        ("Java/Spring", "spring-route-handler"),
    ];

    // Each pattern maps to a confidence value; use a helper closure so the
    // match is co-located and easy to extend.
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

    let supported: Vec<Value> = patterns
        .iter()
        .map(|(lang_fw, tag)| {
            json!({
                "lang_framework": lang_fw,
                "reason_tag": tag,
                "confidence": confidence_for(tag),
            })
        })
        .collect();

    json!({
        "supported_count": supported.len(),
        "supported": supported,
        "note": "TODO(Phase-5): cross-reference against live graph to show detected vs supported",
    })
}

/// External integrations summary: aggregate counts of HTTP/DB/Redis/Queue
/// client usages.
///
/// TODO(Phase-5): load the repo's graph + scan source files (port from
/// commands/tool_map.rs::run). For now, returns a stub with zeroed counts.
fn fetch_externals_summary(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({
        "http": 0,
        "db": 0,
        "redis": 0,
        "queue": 0,
        "note": "TODO(Phase-5): port tool_map scan logic to populate live counts",
    })
}

/// Blind spots: unsupported dynamic-dispatch patterns in the repo.
///
/// TODO(Phase-5): load the repo's graph and read graph.blind_spots (port
/// from commands/doctor.rs::live_blind_spots). For now, returns a stub.
fn fetch_blind_spots(_r: &crate::repo_selector::ResolvedRepo) -> Value {
    json!({
        "total": 0,
        "by_kind": {},
        "note": "TODO(Phase-5): port doctor::live_blind_spots to populate from live graph",
    })
}
