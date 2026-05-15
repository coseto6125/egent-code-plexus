//! `gnx diff` — generalized graph-level cross-commit diff command.
//!
//! Compares two git refs and emits structural changes per requested section
//! (bindings / routes / contracts / all). See spec
//! `docs/superpowers/specs/2026-05-16-tier-b-surface-and-diff-design.md` §5.

use clap::{Args, ValueEnum};
use graph_nexus_core::GnxError;

pub mod baseline;
pub mod bindings;
pub mod git_guard;

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

    if want_bindings {
        let baseline_jsonl = std::env::temp_dir()
            .join(format!("gnx-diff-bindings-baseline-{baseline_sha}.jsonl"));
        let current_jsonl = std::env::temp_dir().join(format!(
            "gnx-diff-bindings-current-{}.jsonl",
            std::process::id()
        ));

        {
            let _guard = git_guard::GitGuard::enter(&repo_dir, &baseline_sha)?;
            bindings::dump(&repo_dir, &baseline_jsonl)?;
        } // _guard dropped here — restores branch + stash

        bindings::dump(&repo_dir, &current_jsonl)?;

        let baseline_map = bindings::load_jsonl(&baseline_jsonl)?;
        let current_map = bindings::load_jsonl(&current_jsonl)?;
        bindings_diff = Some(bindings::diff(&baseline_map, &current_map));

        let _ = std::fs::remove_file(&baseline_jsonl);
        let _ = std::fs::remove_file(&current_jsonl);
    }

    let mut sections = serde_json::Map::new();
    if let Some(bd) = &bindings_diff {
        sections.insert(
            "bindings".into(),
            serde_json::to_value(bd)
                .map_err(|e| GnxError::Output(format!("bindings to_value: {e}")))?,
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
