//! `gnx diff` — generalized graph-level cross-commit diff command.
//!
//! Compares two git refs and emits structural changes per requested section
//! (bindings / routes / contracts / all). See spec
//! `docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md` §5.

use clap::{Args, ValueEnum};
use graph_nexus_core::GnxError;

pub mod baseline;
pub mod bindings;
pub mod contracts;
pub mod git_guard;
pub mod routes;

/// Section of the graph to diff. `All` = bindings + routes + contracts.
#[derive(ValueEnum, Clone, Debug, PartialEq, Eq, Hash)]
#[value(rename_all = "lowercase")]
pub enum DiffSection {
    Bindings,
    Routes,
    Contracts,
    All,
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    /// Comma-separated section(s) to diff: bindings, routes, contracts, or all.
    #[arg(long, value_delimiter = ',', required = true)]
    pub section: Vec<DiffSection>,

    /// Git ref to compare against: branch / tag / commit SHA / HEAD~N / PR/<n>. No default.
    #[arg(long, required = true)]
    pub baseline: String,

    /// Output format: text (default) | json | toon
    #[arg(long, default_value = "text")]
    pub format: String,

    /// List every change (text format only). Default truncates to top-10 per section.
    #[arg(long, default_value_t = false)]
    pub verbose: bool,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: DiffArgs) -> Result<(), GnxError> {
    let repo_dir = match args.repo.as_deref() {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()
            .map_err(|e| GnxError::Output(format!("cwd: {e}")))?,
    };

    let baseline_sha = baseline::resolve(&args.baseline, &repo_dir)?;
    let current_sha = baseline::resolve("HEAD", &repo_dir)?;

    let want_bindings = args
        .section
        .iter()
        .any(|s| matches!(s, DiffSection::Bindings | DiffSection::All));

    let want_routes = args
        .section
        .iter()
        .any(|s| matches!(s, DiffSection::Routes | DiffSection::All));

    let want_contracts = args
        .section
        .iter()
        .any(|s| matches!(s, DiffSection::Contracts | DiffSection::All));

    // Fast-path: identical SHAs → nothing could have changed.
    if baseline_sha == current_sha {
        let mut sections = serde_json::Map::new();
        if want_bindings {
            sections.insert(
                "bindings".into(),
                serde_json::to_value(bindings::BindingsDiff::default())
                    .map_err(|e| GnxError::Output(format!("bindings to_value: {e}")))?,
            );
        }
        if want_routes {
            sections.insert(
                "routes".into(),
                serde_json::to_value(routes::RoutesDiff::default())
                    .map_err(|e| GnxError::Output(format!("routes to_value: {e}")))?,
            );
        }
        if want_contracts {
            sections.insert(
                "contracts".into(),
                serde_json::to_value(contracts::ContractsDiff::default())
                    .map_err(|e| GnxError::Output(format!("contracts to_value: {e}")))?,
            );
        }
        let envelope = serde_json::json!({
            "baseline": {"ref": args.baseline, "sha": baseline_sha},
            "current": {"ref": "HEAD", "sha": current_sha},
            "sections": sections,
        });
        if args.format == "json" {
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope)
                    .map_err(|e| GnxError::Output(format!("json emit: {e}")))?
            );
        } else {
            println!("Bindings diff baseline={} current={} (identical)", baseline_sha, current_sha);
        }
        return Ok(());
    }

    let mut bindings_diff: Option<bindings::BindingsDiff> = None;
    let mut routes_diff: Option<routes::RoutesDiff> = None;
    let mut contracts_diff: Option<contracts::ContractsDiff> = None;

    // Perf note: when both bindings and routes are requested, we call
    // `gnx admin index` twice per state (current + baseline) because bindings
    // uses a temp JSONL dump while routes needs graph.bin directly. A future
    // optimization could share the single index run across sections.
    if want_bindings {
        let baseline_jsonl = std::env::temp_dir()
            .join(format!("gnx-diff-bindings-baseline-{baseline_sha}.jsonl"));
        let current_jsonl = std::env::temp_dir().join(format!(
            "gnx-diff-bindings-current-{}.jsonl",
            std::process::id()
        ));

        bindings::dump(&repo_dir, &current_jsonl)?;

        {
            let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
            bindings::dump(&repo_dir, &baseline_jsonl)?;
        } // _guard dropped here — restores branch + stash

        let baseline_map = bindings::load_jsonl(&baseline_jsonl)?;
        let current_map = bindings::load_jsonl(&current_jsonl)?;
        bindings_diff = Some(bindings::diff(&baseline_map, &current_map));

        let _ = std::fs::remove_file(&baseline_jsonl);
        let _ = std::fs::remove_file(&current_jsonl);
    }

    if want_routes || want_contracts {
        // `bindings::dump` invokes `gnx admin index --repo <dir>` which writes
        // graph.bin as a side effect. We reuse that invocation pattern here via
        // a throwaway JSONL path so we can resolve graph.bin from the registry.
        //
        // Performance concern (surfaced per CLAUDE.md §perf):
        // This re-runs admin index twice (baseline + current), each of which is
        // a full re-analyze. When `want_bindings` is also true that means 4 total
        // admin index runs. A future shared-index refactor would halve this cost.
        //
        // Optimization: routes and contracts share the same graph.bin, so when
        // both are requested we do a single baseline capture for both sections.
        let scratch_current = std::env::temp_dir().join(format!(
            "gnx-diff-graph-scratch-current-{}.jsonl",
            std::process::id()
        ));
        // Build current graph.bin via admin index; ignore the JSONL output.
        bindings::dump(&repo_dir, &scratch_current)?;
        let _ = std::fs::remove_file(&scratch_current);

        // Resolve graph.bin path from registry after indexing current state.
        let legacy_default = std::path::Path::new(".gnx/graph.bin");
        let current_graph = crate::graph_path::resolve(legacy_default, &repo_dir);

        let baseline_graph_tmp = std::env::temp_dir()
            .join(format!("gnx-diff-graph-baseline-{baseline_sha}.bin"));

        {
            let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
            let scratch_baseline = std::env::temp_dir().join(format!(
                "gnx-diff-graph-scratch-baseline-{baseline_sha}.jsonl"
            ));
            bindings::dump(&repo_dir, &scratch_baseline)?;
            let _ = std::fs::remove_file(&scratch_baseline);

            let baseline_graph = crate::graph_path::resolve(legacy_default, &repo_dir);
            std::fs::copy(&baseline_graph, &baseline_graph_tmp).map_err(|e| {
                GnxError::Output(format!(
                    "copy baseline graph {}: {e}",
                    baseline_graph.display()
                ))
            })?;
        } // _guard dropped — restores branch + stash

        if want_routes {
            let current_routes = routes::extract(&current_graph)?;
            let baseline_routes = routes::extract(&baseline_graph_tmp)?;
            routes_diff = Some(routes::diff(&baseline_routes, &current_routes));
        }

        if want_contracts {
            let current_contracts = contracts::extract(&current_graph)?;
            let baseline_contracts = contracts::extract(&baseline_graph_tmp)?;
            contracts_diff = Some(contracts::diff(&baseline_contracts, &current_contracts));
        }

        let _ = std::fs::remove_file(&baseline_graph_tmp);
    }

    let mut sections = serde_json::Map::new();
    if let Some(bd) = &bindings_diff {
        sections.insert(
            "bindings".into(),
            serde_json::to_value(bd)
                .map_err(|e| GnxError::Output(format!("bindings to_value: {e}")))?,
        );
    }
    if let Some(rd) = &routes_diff {
        sections.insert(
            "routes".into(),
            serde_json::to_value(rd)
                .map_err(|e| GnxError::Output(format!("routes to_value: {e}")))?,
        );
    }
    if let Some(cd) = &contracts_diff {
        sections.insert(
            "contracts".into(),
            serde_json::to_value(cd)
                .map_err(|e| GnxError::Output(format!("contracts to_value: {e}")))?,
        );
    }
    let envelope = serde_json::json!({
        "baseline": {"ref": args.baseline, "sha": baseline_sha},
        "current": {"ref": "HEAD", "sha": current_sha},
        "sections": sections,
    });

    if args.format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope)
                .map_err(|e| GnxError::Output(format!("json emit: {e}")))?
        );
    } else {
        // Text fallback — proper formatter lands in Task 12.
        println!(
            "Bindings diff baseline={} current={}",
            baseline_sha, current_sha
        );
        if let Some(bd) = &bindings_diff {
            println!("  new_resolutions: {}", bd.new_resolutions.len());
            println!("  tier_changes:    {}", bd.tier_changes.len());
            println!("  target_changes:  {}", bd.target_changes.len());
            println!("  removed:         {}", bd.removed.len());
        }
    }
    Ok(())
}
