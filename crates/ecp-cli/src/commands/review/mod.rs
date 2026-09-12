//! `ecp review` — LLM-workflow audit aggregator (default) +
//! provable-verdict layer (with `--verdicts`).
//!
//! Default mode: calls each constituent's `build_payload` library fn,
//! maps results to `Finding` rows, filters to high-confidence signal only.
//! `--verdicts` mode: runs `ecp diff --section all` internally and emits
//! a flat verdict list derived from the section deltas — every verdict
//! cites the exact section / record that triggered it. See `verdicts.rs`.

use crate::commands::diff::{self, DiffArgs, DiffSection};
use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use ecp_core::EcpError;

pub mod aggregate;
pub mod findings;
pub mod flow;
pub mod scope;
pub mod verdicts;

#[derive(clap::ValueEnum, Debug, Clone, PartialEq, Eq)]
pub enum ReviewInclude {
    Flow,
}

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    /// Git ref to diff against. Defaults to working-tree changes (HEAD).
    #[arg(long, alias = "baseline")]
    pub since: Option<String>,

    /// Include source-based analysis alongside existing findings.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub include: Vec<ReviewInclude>,

    /// Explicit file list (comma-separated). Overrides --since.
    #[arg(long, value_delimiter = ',')]
    pub files: Option<Vec<String>>,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: toon (default) | json
    #[arg(long)]
    pub format: Option<String>,

    /// Emit provable verdicts derived from `ecp diff --section all` instead
    /// of the per-file aggregate findings. Requires `--since <ref>` (the
    /// baseline). Output shape: `{baseline, current, verdicts, elapsed_ms}`.
    #[arg(long, default_value_t = false)]
    pub verdicts: bool,
}

pub fn run(args: ReviewArgs, engine: &Engine) -> Result<(), EcpError> {
    if args.verdicts && !args.include.is_empty() {
        return Err(EcpError::InvalidArgument(
            "--include flow cannot be combined with --verdicts".into(),
        ));
    }
    if args.verdicts {
        return run_verdicts(&args);
    }
    let start = std::time::Instant::now();
    let repo_dir = match args.repo.as_deref() {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            std::env::current_dir().map_err(|e| EcpError::Output(format!("resolve cwd: {e}")))?
        }
    };
    let files = scope::resolve(&args, &repo_dir)?;
    let report = aggregate::run(&files, &repo_dir, engine, args.since.as_deref())?;
    let mut payload = report.emit(start.elapsed());
    if args.include.contains(&ReviewInclude::Flow) {
        payload["flow"] = match flow::build(
            &repo_dir,
            args.since.as_deref().unwrap_or("HEAD"),
            args.files.as_deref(),
        ) {
            Ok(flow) => flow,
            // The graph review stays usable; the flow slice reports its own gap.
            Err(error) => serde_json::json!({
                "analysis": [],
                "truncated": true,
                "unresolved": error.to_string(),
            }),
        };
        if payload["flow"]["analysis"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
            || payload["flow"]["truncated"].as_bool() == Some(true)
        {
            payload["legacy_status"] = payload["status"].clone();
            payload["status"] = serde_json::json!("review_required");
        }
        payload["elapsed_ms"] = serde_json::json!(start.elapsed().as_millis() as u64);
    }
    let format = OutputFormat::parse(args.format.as_deref());
    emit(&payload, format)
}

fn run_verdicts(args: &ReviewArgs) -> Result<(), EcpError> {
    let start = std::time::Instant::now();
    let since = args
        .since
        .as_deref()
        .ok_or_else(|| EcpError::Output("--verdicts requires --since <ref> (baseline)".into()))?;
    let diff_args = DiffArgs {
        section: vec![
            DiffSection::Bindings,
            DiffSection::Routes,
            DiffSection::Contracts,
            DiffSection::Symbols,
        ],
        baseline: Some(since.to_string()),
        baseline_graph: None,
        current_graph: None,
        format: None,
        verbose: false,
        repo: args.repo.clone(),
    };
    let payload = diff::build_payload(&diff_args)?;
    let verdicts = verdicts::derive(&payload);
    let json = serde_json::json!({
        "baseline": {"ref": payload.baseline_ref, "sha": payload.baseline_sha},
        "current":  {"ref": payload.current_ref,  "sha": payload.current_sha},
        "verdicts": verdicts,
        "summary": {
            "total":     verdicts.len(),
            "risk":      verdicts.iter().filter(|v| matches!(v.severity, verdicts::Severity::Risk)).count(),
            "warn":      verdicts.iter().filter(|v| matches!(v.severity, verdicts::Severity::Warn)).count(),
            "info":      verdicts.iter().filter(|v| matches!(v.severity, verdicts::Severity::Info)).count(),
        },
        "elapsed_ms": start.elapsed().as_millis() as u64,
    });
    let format = OutputFormat::parse(args.format.as_deref());
    emit(&json, format)
}
