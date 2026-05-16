use crate::git::safe_exec;
use graph_nexus_core::registry::sanitize_segment;
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

/// Compute the stable per-repo directory name under `~/.gnx/`.
///
/// Identity rule: `<sanitize(basename(common_dir))>__<sha256(canonical_common_dir)[:8]>`.
/// All worktrees of the same git repo share the same `--git-common-dir`,
/// so this naturally collapses `git worktree add` siblings onto a single
/// `<repo>/` namespace — solving v1's per-worktree duplication.
pub fn repo_dir_name_for_cwd(cwd: &Path) -> io::Result<String> {
    let common_dir = git_common_dir(cwd)?;
    let canonical = std::fs::canonicalize(&common_dir)?;

    // basename derivation: parent of `.git` is the repo root; if common_dir
    // is bare (e.g. ends with `.git` and has no enclosing dir), fall back to
    // common_dir's own basename.
    let basename = canonical
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| canonical.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let cleaned = basename.trim_start_matches(['.', '-']);
    let safe = sanitize_segment(if cleaned.is_empty() { "repo" } else { cleaned })
        .unwrap_or_else(|_| "repo".to_string());
    let h = sha256_hex8(canonical.to_string_lossy().as_bytes());
    Ok(format!("{safe}__{h}"))
}

fn git_common_dir(cwd: &Path) -> io::Result<std::path::PathBuf> {
    let out = safe_exec::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("not a git repository"));
    }
    let path_str = std::str::from_utf8(&out.stdout)
        .map_err(io::Error::other)?
        .trim();
    let p = std::path::PathBuf::from(path_str);
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(cwd.join(p))
    }
}

fn sha256_hex8(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(&digest[..4])
}
