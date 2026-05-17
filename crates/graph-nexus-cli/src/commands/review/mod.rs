//! `gnx review` — LLM-workflow audit aggregator.
//!
//! One command, one report. Calls each constituent's `build_payload`
//! library fn, maps results to `Finding` rows, filters to high-confidence
//! signal only, emits per spec.

use crate::engine::Engine;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;

pub mod aggregate;
pub mod findings;
pub mod scope;

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    /// Git ref to diff against. Defaults to working-tree changes (HEAD).
    #[arg(long)]
    pub since: Option<String>,

    /// Explicit file list (comma-separated). Overrides --since.
    #[arg(long, value_delimiter = ',')]
    pub files: Option<Vec<String>>,

    /// Repository root path (defaults to current directory).
    #[arg(long)]
    pub repo: Option<String>,

    /// Output format: toon (default) | json
    #[arg(long)]
    pub format: Option<String>,
}

pub fn run(args: ReviewArgs, engine: &Engine) -> Result<(), GnxError> {
    let start = std::time::Instant::now();
    let repo_dir = args
        .repo
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let files = scope::resolve(&args, &repo_dir)?;
    let report = aggregate::run(&files, &repo_dir, engine, args.since.as_deref())?;
    let payload = report.emit(start.elapsed());
    let format = OutputFormat::parse(args.format.as_deref());
    emit(&payload, format)
}
