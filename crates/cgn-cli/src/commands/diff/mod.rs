//! `cgn diff` — generalized graph-level cross-commit diff command.
//!
//! Compares two git refs and emits structural changes per requested section
//! (bindings / routes / contracts / all). See spec
//! `docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md` §5.

use clap::{Args, ValueEnum};
use cgn_core::GnxError;

pub mod baseline;
pub mod bindings;
pub mod contracts;
pub mod git_guard;
pub mod output;
pub mod routes;

#[derive(Debug)]
pub struct DiffPayload {
    pub bindings: Option<bindings::BindingsDiff>,
    pub routes: Option<routes::RoutesDiff>,
    pub contracts: Option<contracts::ContractsDiff>,
    pub baseline_ref: String,
    pub baseline_sha: String,
    pub current_ref: String,
    pub current_sha: String,
    pub verbose: bool,
}

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

    /// Output format. Omit for the LLM-tuned default; pass
    /// `--format text|json|toon` for the alternative renderings.
    #[arg(long)]
    pub format: Option<String>,

    /// List every change (text format only). Default truncates to top-10 per section.
    #[arg(long, default_value_t = false)]
    pub verbose: bool,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: DiffArgs) -> Result<(), GnxError> {
    let payload = build_payload(&args)?;
    let format = args.format.as_deref().unwrap_or("");
    output::emit(&payload, format)
}

pub fn build_payload(args: &DiffArgs) -> Result<DiffPayload, GnxError> {
    let repo_dir = match args.repo.as_deref() {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| GnxError::Output(format!("cwd: {e}")))?,
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
        return Ok(DiffPayload {
            bindings: want_bindings.then(bindings::BindingsDiff::default),
            routes: want_routes.then(routes::RoutesDiff::default),
            contracts: want_contracts.then(contracts::ContractsDiff::default),
            baseline_ref: args.baseline.clone(),
            baseline_sha,
            current_ref: "HEAD".to_string(),
            current_sha,
            verbose: args.verbose,
        });
    }

    let mut bindings_diff: Option<bindings::BindingsDiff> = None;
    let mut routes_diff: Option<routes::RoutesDiff> = None;
    let mut contracts_diff: Option<contracts::ContractsDiff> = None;

    // Single admin index per state (current + baseline). The dump writes
    // both the resolver JSONL (for bindings) and graph.bin (for routes /
    // contracts) as side effects of one analyze pass — so we run it ONCE
    // per state regardless of which sections are requested.
    let current_jsonl =
        std::env::temp_dir().join(format!("cgn-diff-current-{}.jsonl", std::process::id()));
    let baseline_jsonl =
        std::env::temp_dir().join(format!("cgn-diff-baseline-{baseline_sha}.jsonl"));
    let baseline_graph_tmp =
        std::env::temp_dir().join(format!("cgn-diff-graph-baseline-{baseline_sha}.bin"));
    let legacy_default = std::path::Path::new(".gnx/graph.bin");

    bindings::dump(&repo_dir, &current_jsonl)?;
    let current_graph = crate::graph_path::resolve(legacy_default, &repo_dir);

    {
        let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
        bindings::dump(&repo_dir, &baseline_jsonl)?;
        let baseline_graph = crate::graph_path::resolve(legacy_default, &repo_dir);
        std::fs::copy(&baseline_graph, &baseline_graph_tmp).map_err(|e| {
            GnxError::Output(format!(
                "copy baseline graph {}: {e}",
                baseline_graph.display()
            ))
        })?;
    } // _guard drops — branch + stash restored

    if want_bindings {
        let baseline_map = bindings::load_jsonl(&baseline_jsonl)?;
        let current_map = bindings::load_jsonl(&current_jsonl)?;
        bindings_diff = Some(bindings::diff(&baseline_map, &current_map));
    }

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

    let _ = std::fs::remove_file(&current_jsonl);
    let _ = std::fs::remove_file(&baseline_jsonl);
    let _ = std::fs::remove_file(&baseline_graph_tmp);

    Ok(DiffPayload {
        bindings: bindings_diff,
        routes: routes_diff,
        contracts: contracts_diff,
        baseline_ref: args.baseline.clone(),
        baseline_sha,
        current_ref: "HEAD".to_string(),
        current_sha,
        verbose: args.verbose,
    })
}
