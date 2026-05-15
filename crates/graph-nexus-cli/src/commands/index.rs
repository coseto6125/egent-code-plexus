//! `gnx index <path>` — registry recovery for an existing on-disk index.
//!
//! Re-registers a repo whose `.gitnexus-rs/graph.bin` exists on disk but
//! is missing from `~/.gnx/registry.json` (e.g. `gnx analyze` crashed
//! after writing graph.bin but before completing the registry update).
//! Subsequent `gnx context`/`impact`/etc. can then resolve the repo by
//! name again.

use crate::git_state;
use clap::Args;
use graph_nexus_core::registry::{sanitize_segment, AuditEvent, AuditLog, Registry, RepoEntry};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct IndexArgs {
    /// Path to an existing repo whose `.gitnexus-rs/` folder should be
    /// (re-)registered. Defaults to the current directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

pub fn run(args: IndexArgs) -> Result<(), graph_nexus_core::GnxError> {
    // 1. Canonicalize the path; require it to be an existing directory.
    let abs_path = args.path.canonicalize().map_err(|e| {
        graph_nexus_core::GnxError::InvalidArgument(format!("path {:?}: {e}", args.path))
    })?;
    if !abs_path.is_dir() {
        return Err(graph_nexus_core::GnxError::InvalidArgument(format!(
            "path {:?} is not a directory",
            abs_path
        )));
    }

    // 2. Require .gitnexus-rs/graph.bin under the path.
    let graph_bin = abs_path.join(".gitnexus-rs").join("graph.bin");
    if !graph_bin.exists() {
        return Err(graph_nexus_core::GnxError::InvalidArgument(format!(
            "no .gitnexus-rs/graph.bin at {} — run `gnx analyze` first",
            abs_path.display()
        )));
    }

    // 3. Derive (repo_name, branch). Friendlier UX: if the path isn't a
    //    git repo, fall back to the basename + "(none)" instead of
    //    erroring out — recovery should still work on detached or
    //    moved snapshots.
    let (repo_name, branch) = match git_state::resolve(&abs_path) {
        Ok(state) => (state.repo_name, state.branch),
        Err(_) => {
            let basename = abs_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .trim_start_matches(['.', '-']);
            let candidate = if basename.is_empty() {
                "unknown"
            } else {
                basename
            };
            let name = sanitize_segment(candidate).map_err(|e| {
                graph_nexus_core::GnxError::InvalidArgument(format!(
                    "repo_name from path basename: {e}"
                ))
            })?;
            (name, "(none)".to_string())
        }
    };

    // 4. Open the registry.
    let home_gnx = graph_nexus_core::registry::resolve_home_gnx();
    let mut registry = Registry::open(&home_gnx)
        .map_err(|e| graph_nexus_core::GnxError::InvalidArgument(format!("registry open: {e}")))?;

    // 5. Build a RepoEntry pointing at the absolute path. Preserve
    //    existing branches if any entry already exists under this name.
    let worktree_path = abs_path.to_string_lossy().to_string();
    let index_dir_root = abs_path.join(".gitnexus-rs").to_string_lossy().to_string();
    let branches = registry
        .snapshot()
        .repos
        .iter()
        .find(|r| r.name == repo_name)
        .map(|r| r.branches.clone())
        .unwrap_or_default();
    let entry = RepoEntry {
        name: repo_name.clone(),
        remote_url: String::new(),
        worktree_path: worktree_path.clone(),
        index_dir_root,
        branches,
        groups: vec![],
    };
    registry.upsert_repo(entry).map_err(|e| {
        graph_nexus_core::GnxError::InvalidArgument(format!("registry upsert: {e}"))
    })?;

    // 6. Audit. AuditEvent has no dedicated "Index" variant; record this
    //    recovery as a HookFired{kind: "index"} so downstream log readers
    //    can grep it the same way they already do for clean/prune.
    if let Ok(audit) = AuditLog::open(&home_gnx.join("audit.log")) {
        let _ = audit.append(&AuditEvent::HookFired {
            kind: "index".into(),
            from: None,
            to: Some(branch),
            repo: repo_name.clone(),
        });
    }

    // 7. One-line confirmation.
    println!("registered {} @ {}", repo_name, worktree_path);
    Ok(())
}
