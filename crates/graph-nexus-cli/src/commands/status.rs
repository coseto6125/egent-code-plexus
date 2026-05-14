use crate::git::safe_exec;
use crate::output::{emit, OutputFormat};
use clap::Args;
use graph_nexus_core::GnxError;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long, default_value = "json")]
    pub format: Option<String>,
}

pub fn run(args: StatusArgs) -> Result<(), GnxError> {
    let cwd = PathBuf::from(args.repo.as_deref().unwrap_or("."));

    let git_state = crate::git_state::resolve(&cwd)
        .map_err(|e| GnxError::InvalidArgument(format!("Git error: {e}")))?;

    let graph_path = crate::graph_path::resolve(std::path::Path::new(".gitnexus-rs/graph.bin"), &cwd);

    let mut graph_mtime = 0;
    let mut graph_exists = false;
    if graph_path.exists() {
        graph_exists = true;
        if let Ok(meta) = std::fs::metadata(&graph_path) {
            if let Ok(mtime) = meta.modified() {
                graph_mtime = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }
        }
    }

    let mut commit_time = 0;
    let output = safe_exec::git()
        .args(["log", "-1", "--format=%ct"])
        .current_dir(&cwd)
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Ok(t) = s.trim().parse::<u64>() {
                commit_time = t;
            }
        }
    }

    let mut is_dirty = false;
    let output = safe_exec::git()
        .args(["status", "--porcelain"])
        .current_dir(&cwd)
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if !s.trim().is_empty() {
                is_dirty = true;
            }
        }
    }

    let is_stale = !graph_exists || commit_time > graph_mtime || is_dirty;

    let mut text_lines = Vec::new();
    text_lines.push(format!("Repo: {}", git_state.repo_name));
    text_lines.push(format!("Branch: {}", git_state.branch));
    text_lines.push(format!("Graph Exists: {}", graph_exists));
    text_lines.push(format!("Graph MTime: {}", graph_mtime));
    text_lines.push(format!("Commit Time: {}", commit_time));
    text_lines.push(format!("Is Dirty: {}", is_dirty));
    text_lines.push(format!("Is Stale: {}", is_stale));

    let result = serde_json::json!({
        "repo_name": git_state.repo_name,
        "branch": git_state.branch,
        "worktree_path": git_state.worktree_path.to_string_lossy().to_string(),
        "graph_path": graph_path.to_string_lossy().to_string(),
        "graph_exists": graph_exists,
        "graph_mtime": graph_mtime,
        "commit_time": commit_time,
        "is_dirty": is_dirty,
        "is_stale": is_stale,
        "results": text_lines,
    });

    emit(&result, OutputFormat::parse(args.format.as_deref()))
}
