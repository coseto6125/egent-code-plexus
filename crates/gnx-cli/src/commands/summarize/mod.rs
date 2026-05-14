//! `gnx summarize` — LLM-friendly project overview.
//!
//! 對應 design doc: docs/superpowers/specs/2026-05-14-gnx-summarize-design.md
//! 採 D 變體：分層 (top hot files → architecture/communities → per-file detail)
//! + 跳過 in_deg=0 孤兒 + 同名符號補 "shadowed by N" 提示。

pub mod analysis;
pub mod ranking;
pub mod render;

use crate::engine::Engine;
use clap::Args;
use gnx_core::GnxError;

#[derive(Args, Debug, Clone)]
pub struct SummarizeArgs {
    /// Repository name or path (for multi-repo registry resolution).
    #[arg(long)]
    pub repo: Option<String>,

    /// Top-N hottest files in summary (sorted by aggregated in-degree).
    #[arg(long, default_value_t = 10)]
    pub top_files: usize,

    /// Top-N communities in architecture section.
    #[arg(long, default_value_t = 10)]
    pub top_communities: usize,

    /// Top-N symbols per file in per-file detail section.
    #[arg(long, default_value_t = 3)]
    pub top_symbols: usize,

    /// Output format: md (default) or json.
    #[arg(long, default_value = "md")]
    pub format: String,

    /// Write to file instead of stdout.
    #[arg(long)]
    pub output: Option<String>,

    /// Keep in_deg=0 && out_deg=0 orphan symbols (off by default to reduce noise).
    #[arg(long, default_value_t = false)]
    pub include_orphans: bool,
}

pub fn run(args: SummarizeArgs, engine: &Engine) -> Result<(), GnxError> {
    let g = engine.graph().map_err(|e| GnxError::Rkyv(e.to_string()))?;

    let stats = analysis::degree_stats(g);
    let by_file = analysis::by_file(g);
    let by_community = analysis::by_community(g);
    let name_collisions = analysis::name_collisions(g);

    let top_files = ranking::top_files(&by_file, &stats, args.top_files);
    let top_communities = ranking::top_communities(g, &by_community, args.top_communities);

    let input = render::RenderInput {
        graph: g,
        stats: &stats,
        by_file: &by_file,
        top_files: &top_files,
        top_communities: &top_communities,
        total_communities: by_community.len(),
        name_collisions: &name_collisions,
        top_symbols_per_file: args.top_symbols,
        exclude_orphans: !args.include_orphans,
    };

    let text = match args.format.as_str() {
        "md" | "markdown" => render::markdown(&input),
        "json" => serde_json::to_string_pretty(&render::json(&input))
            .map_err(|e| GnxError::Rkyv(format!("json serialize: {e}")))?,
        other => {
            return Err(GnxError::Rkyv(format!(
                "unsupported --format '{other}' (expected: md, json)"
            )));
        }
    };

    match args.output.as_deref() {
        Some(path) => {
            std::fs::write(path, &text).map_err(|e| GnxError::Rkyv(format!("write {path}: {e}")))?
        }
        None => print!("{text}"),
    }
    Ok(())
}
