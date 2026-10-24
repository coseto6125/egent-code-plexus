//! `ecp diff` — generalized graph-level cross-commit diff command.
//!
//! Compares two git refs and emits structural changes per requested section
//! (bindings / routes / contracts / all). See spec
//! `docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md` §5.

use crate::output::OutputFormat;
use clap::{Args, ValueEnum};
use ecp_core::EcpError;
use std::path::PathBuf;

pub mod baseline;
pub mod baseline_checkout;
pub mod bindings;
pub mod contracts;
pub mod output;
pub mod routes;
pub mod symbols;

#[derive(Debug)]
pub struct DiffPayload {
    pub bindings: Option<bindings::BindingsDiff>,
    pub routes: Option<routes::RoutesDiff>,
    pub contracts: Option<contracts::ContractsDiff>,
    pub symbols: Option<symbols::SymbolsDiff>,
    pub baseline_ref: String,
    pub baseline_sha: String,
    pub current_ref: String,
    pub current_sha: String,
    pub verbose: bool,
}

/// Section of the graph to diff. `All` = bindings + routes + contracts + symbols.
#[derive(ValueEnum, Clone, Debug, PartialEq, Eq, Hash)]
#[value(rename_all = "lowercase")]
pub enum DiffSection {
    Bindings,
    Routes,
    Contracts,
    Symbols,
    All,
}

#[derive(Args, Debug, Clone)]
pub struct DiffArgs {
    /// Comma-separated section(s) to diff: bindings, routes, contracts, symbols, or all.
    #[arg(long, value_delimiter = ',', required = true)]
    pub section: Vec<DiffSection>,

    /// Git ref to compare against: branch / tag / commit SHA / HEAD~N / PR/<n>.
    /// Required unless `--baseline-graph` is supplied (A-mode snapshot diff).
    #[arg(long, required_unless_present = "baseline_graph")]
    pub baseline: Option<String>,

    /// A-mode: path to baseline `graph.bin` (skip git checkout + re-index).
    /// When set, requires `--current-graph` and restricts sections to those
    /// that read directly from graph.bin (routes / contracts / symbols).
    #[arg(long, conflicts_with = "baseline", requires = "current_graph")]
    pub baseline_graph: Option<PathBuf>,

    /// A-mode: path to current `graph.bin`. Required when `--baseline-graph`
    /// is supplied.
    #[arg(long, requires = "baseline_graph")]
    pub current_graph: Option<PathBuf>,

    /// Output format. Absent or `llm`: the LLM-tuned toon payload. `toon`: uncompressed
    /// toon. `json`: full-fidelity JSON. `text`: per-section change counts and up to 10
    /// changes per kind (all with `--verbose`).
    #[arg(long)]
    pub format: Option<OutputFormat>,

    /// List every change (text format only). Default truncates to top-10 per section.
    #[arg(long, default_value_t = false)]
    pub verbose: bool,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: DiffArgs) -> Result<(), EcpError> {
    let payload = build_payload(&args)?;
    output::emit(&payload, args.format.unwrap_or_default())
}

pub fn build_payload(args: &DiffArgs) -> Result<DiffPayload, EcpError> {
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
    let want_symbols = args
        .section
        .iter()
        .any(|s| matches!(s, DiffSection::Symbols | DiffSection::All));

    // ── A-mode: two graph.bin paths, no git checkout ────────────────────
    if let (Some(bg), Some(cg)) = (args.baseline_graph.as_ref(), args.current_graph.as_ref()) {
        if want_bindings {
            return Err(EcpError::Output(
                "--section bindings cannot run in --baseline-graph mode \
                 (bindings needs a resolver JSONL dump, not graph.bin)"
                    .into(),
            ));
        }
        let routes_diff = if want_routes {
            let b = routes::extract(bg)?;
            let c = routes::extract(cg)?;
            Some(routes::diff(&b, &c))
        } else {
            None
        };
        let contracts_diff = if want_contracts {
            let b = contracts::extract(bg)?;
            let c = contracts::extract(cg)?;
            Some(contracts::diff(&b, &c))
        } else {
            None
        };
        let symbols_diff = if want_symbols {
            let b = symbols::extract(bg)?;
            let c = symbols::extract(cg)?;
            Some(symbols::diff(&b, &c)?)
        } else {
            None
        };
        return Ok(DiffPayload {
            bindings: None,
            routes: routes_diff,
            contracts: contracts_diff,
            symbols: symbols_diff,
            baseline_ref: bg.display().to_string(),
            baseline_sha: String::new(),
            current_ref: cg.display().to_string(),
            current_sha: String::new(),
            verbose: args.verbose,
        });
    }

    // ── B/C-mode: --baseline <ref> (or PR/<n>) ──────────────────────────
    let baseline_ref = args
        .baseline
        .as_deref()
        .ok_or_else(|| EcpError::Output("--baseline or --baseline-graph required".into()))?;

    let repo_dir = match args.repo.as_deref() {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| EcpError::Output(format!("cwd: {e}")))?,
    };

    let baseline_sha = baseline::resolve(baseline_ref, &repo_dir)?;
    let current_sha = baseline::resolve("HEAD", &repo_dir)?;

    // Fast-path: identical SHAs → nothing could have changed.
    if baseline_sha == current_sha {
        return Ok(DiffPayload {
            bindings: want_bindings.then(bindings::BindingsDiff::default),
            routes: want_routes.then(routes::RoutesDiff::default),
            contracts: want_contracts.then(contracts::ContractsDiff::default),
            symbols: want_symbols.then(symbols::SymbolsDiff::default),
            baseline_ref: baseline_ref.to_string(),
            baseline_sha,
            current_ref: "HEAD".to_string(),
            current_sha,
            verbose: args.verbose,
        });
    }

    let mut bindings_diff: Option<bindings::BindingsDiff> = None;
    let mut routes_diff: Option<routes::RoutesDiff> = None;
    let mut contracts_diff: Option<contracts::ContractsDiff> = None;
    let mut symbols_diff: Option<symbols::SymbolsDiff> = None;

    // Single admin index per state (current + baseline). The dump writes
    // both the resolver JSONL (for bindings) and graph.bin (for routes /
    // contracts / symbols) as side effects of one analyze pass — so we run
    // it ONCE per state regardless of which sections are requested.
    // Built from a scratch checkout, never from the user's tree: stashing and
    // checking out there loses staged state and races a second `ecp diff`.
    let baseline_checkout = baseline_checkout::BaselineCheckout::create(&repo_dir, &baseline_sha)?;
    let current_jsonl = baseline_checkout.scratch().join("current.jsonl");
    let baseline_jsonl = baseline_checkout.scratch().join("baseline.jsonl");
    let baseline_graph_tmp = baseline_checkout.scratch().join("baseline-graph.bin");

    bindings::dump(&repo_dir, &current_jsonl)?;
    // Current side: full ensure (ecp-version fingerprint → rebuild; dirty
    // tree → incremental) so the diff's current snapshot can't be a stale-
    // binary graph that reports phantom adds/removes against the baseline.
    let current_graph = ensure_graph_synchronously(&repo_dir, None, "current")?;

    {
        let baseline_dir = baseline_checkout.path();
        bindings::dump(&baseline_dir, &baseline_jsonl)?;
        // Baseline side: attach only to a graph of the baseline SHA that this
        // ecp built — a fingerprint drift here means the cached baseline graph
        // was built by an older ecp, which would surface as a phantom diff —
        // else rebuild it before the copy.
        let user_root = crate::build::orchestrator::worktree_toplevel(&repo_dir)
            .unwrap_or_else(|| repo_dir.clone());
        let baseline_graph = ensure_baseline_graph(&baseline_dir, &user_root, &baseline_sha)?;
        std::fs::copy(&baseline_graph, &baseline_graph_tmp).map_err(|e| {
            EcpError::Output(format!(
                "copy baseline graph {}: {e}",
                baseline_graph.display()
            ))
        })?;
    }

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

    if want_symbols {
        let baseline_snap = symbols::extract(&baseline_graph_tmp)?;
        let current_snap = symbols::extract(&current_graph)?;
        symbols_diff = Some(symbols::diff(&baseline_snap, &current_snap)?);
    }

    drop(baseline_checkout);

    Ok(DiffPayload {
        bindings: bindings_diff,
        routes: routes_diff,
        contracts: contracts_diff,
        symbols: symbols_diff,
        baseline_ref: baseline_ref.to_string(),
        baseline_sha,
        current_ref: "HEAD".to_string(),
        current_sha,
        verbose: args.verbose,
    })
}

/// Ensure the graph for the currently-checked-out tree is published and
/// synchronously on disk, then return its resolved path.
///
/// `sha` is `None` for the current side (HEAD). The path is resolved *after*
/// the ensure so a freshly published commit dir wins over the legacy
/// `.ecp/graph.bin`.
fn ensure_graph_synchronously(
    repo_dir: &std::path::Path,
    sha: Option<&str>,
    label: &str,
) -> Result<std::path::PathBuf, EcpError> {
    let legacy_default = std::path::Path::new(".ecp/graph.bin");
    let probe = crate::graph_path::resolve(legacy_default, repo_dir);
    crate::auto_ensure::ensure_fresh(
        crate::auto_ensure::IndexNeed::ExactSha(sha),
        &probe,
        repo_dir,
    )
    .map_err(|e| EcpError::Output(format!("ensure {label} graph: {e}")))?;
    Ok(crate::graph_path::resolve(legacy_default, repo_dir))
}

/// The baseline side's `graph.bin`: the published graph of `sha` when this
/// ecp built it, else a build from `checkout`. The checkout is deleted when
/// the diff ends, so the build records `user_root` as its worktree; the next
/// diff of the same baseline then attaches instead of indexing it again.
fn ensure_baseline_graph(
    checkout: &std::path::Path,
    user_root: &std::path::Path,
    sha: &str,
) -> Result<std::path::PathBuf, EcpError> {
    let start = std::time::Instant::now();
    let mut built = crate::build::orchestrator::build_l2_recorded(checkout, user_root, sha)
        .map_err(|e| EcpError::Output(format!("ensure baseline graph: {e}")))?;
    if built.tantivy_handle.is_some() {
        eprintln!("l2.built elapsed={:.2}s", start.elapsed().as_secs_f32());
    }
    // Joined here: nothing else keeps this process alive for the writer, and
    // a writer cut off at exit leaves its temp dir in the published graph.
    built.join_background();
    Ok(built.commit_dir.join("graph.bin"))
}
