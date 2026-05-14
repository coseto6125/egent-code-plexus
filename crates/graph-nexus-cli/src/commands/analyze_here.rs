//! `gnx analyze-here` — convenience wrapper around `gnx analyze --repo $(pwd)`.
//!
//! Lets an LLM working inside a cloned worktree index the repo without
//! having to spell out the absolute path. Strict pass-through: every flag
//! mirrors `analyze::AnalyzeArgs` so behavior stays in one place.

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct AnalyzeHereArgs {
    /// Mirror of `gnx analyze --embeddings`.
    #[arg(long, default_value_t = false)]
    pub embeddings: bool,

    /// Mirror of `gnx analyze --drop-embeddings`.
    #[arg(long, default_value_t = false)]
    pub drop_embeddings: bool,

    /// Mirror of `gnx analyze --force`.
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Mirror of `gnx analyze --dump-resolver`.
    #[arg(long)]
    pub dump_resolver: Option<std::path::PathBuf>,

    /// Mirror of `gnx analyze --no-cache`.
    #[arg(long, default_value_t = false)]
    pub no_cache: bool,
}

/// Run `analyze` with `repo` set to the current working directory.
/// All other behavior comes from `analyze::run`.
pub fn run(args: AnalyzeHereArgs) -> Result<(), String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("cwd: {e}"))?
        .to_string_lossy()
        .into_owned();
    crate::commands::analyze::run(crate::commands::analyze::AnalyzeArgs {
        repo: cwd,
        embeddings: args.embeddings,
        drop_embeddings: args.drop_embeddings,
        force: args.force,
        dump_resolver: args.dump_resolver,
        no_cache: args.no_cache,
    })
}
