//! Path sanitization, repo/branch derivation, UID path normalization.

use thiserror::Error;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Error)]
pub enum PathError {
    #[error("segment is empty")]
    Empty,
    #[error("segment exceeds 64 chars: {0}")]
    TooLong(String),
    #[error("segment contains illegal char or pattern: {0}")]
    Illegal(String),
}

/// Validate a single path segment (e.g. `<repo>` or `<branch>`) for use
/// inside `~/.gnx/`. Whitelist `[A-Za-z0-9_.-]+`, reject `..`, reject
/// leading `-` or `.`, max 64 chars.
pub fn sanitize_segment(s: &str) -> Result<String, PathError> {
    if s.is_empty() {
        return Err(PathError::Empty);
    }
    if s.len() > 64 {
        return Err(PathError::TooLong(s.to_string()));
    }
    if s.contains("..") || s.starts_with('-') || s.starts_with('.') {
        return Err(PathError::Illegal(s.to_string()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(PathError::Illegal(s.to_string()));
    }
    Ok(s.to_string())
}

/// Sanitize a git branch name for use as a directory segment.
/// Maps `/` → `__`, other illegal chars → `_`, then applies
/// `sanitize_segment` rules.
pub fn sanitize_branch(branch: &str) -> Result<String, PathError> {
    if branch.is_empty() {
        return Err(PathError::Empty);
    }
    let replaced: String = branch
        .chars()
        .flat_map(|c| match c {
            '/' => "__".chars().collect::<Vec<_>>(),
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') => vec![c],
            _ => vec!['_'],
        })
        .collect();
    sanitize_segment(&replaced)
}

/// Extract `<repo>` segment from a git remote URL. Handles SSH
/// (`git@host:user/repo.git`) and HTTPS (`https://host/user/repo.git`).
/// `None` returns Err (caller falls back to working-tree basename).
pub fn derive_repo_name(remote_url: Option<&str>) -> Result<String, PathError> {
    let url = remote_url.ok_or(PathError::Empty)?;
    // Reject if the entire URL contains suspicious path traversal patterns
    if url.contains("..") || url.contains("/../") {
        return Err(PathError::Illegal(url.to_string()));
    }
    let after_colon_or_slash = url
        .rsplit_once(|c| c == ':' || c == '/')
        .map(|(_, tail)| tail)
        .unwrap_or(url);
    let stripped = after_colon_or_slash
        .strip_suffix(".git")
        .unwrap_or(after_colon_or_slash);
    sanitize_segment(stripped)
}

/// Cross-platform stable UID path: repo-relative, forward-slash, NFC.
/// Returns Err if `absolute` isn't under `repo_root`.
pub fn uid_path(absolute: &Path, repo_root: &Path) -> Result<String, PathError> {
    let rel = absolute
        .strip_prefix(repo_root)
        .map_err(|_| PathError::Illegal(format!("{absolute:?} not under {repo_root:?}")))?;
    let s = rel.to_string_lossy().replace('\\', "/");
    Ok(s.nfc().collect())
}

/// Resolved layout for one (repo, branch, worktree_path) triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLayout {
    pub index_dir: PathBuf,
    /// `Some("a1b2c3d4")` if collision required hash suffix.
    pub disambiguator: Option<String>,
}

impl IndexLayout {
    /// Resolve `~/.gnx/<repo>/<branch>/` or, on collision, append an
    /// 8-char hash of the canonical worktree path.
    pub fn resolve(
        home_gnx: &Path,
        repo_name: &str,
        branch: &str,
        worktree_path: &str,
        existing_repos: &[(String, String)],
    ) -> Result<Self, PathError> {
        let repo = sanitize_segment(repo_name)?;
        let br = sanitize_branch(branch)?;

        let collides = existing_repos
            .iter()
            .any(|(r, w)| r == &repo && w != worktree_path);

        let (index_dir, disambiguator) = if collides {
            let hash = hash8(worktree_path);
            let dir_name = format!("{repo}-{hash}");
            (home_gnx.join(&dir_name).join(&br), Some(hash))
        } else {
            (home_gnx.join(&repo).join(&br), None)
        };

        // Defense in depth: ensure the computed index_dir is rooted in home_gnx
        // even after symlink expansion (spec §8 C1).
        if let Ok(canonical_home) = home_gnx.canonicalize() {
            let test_str = index_dir.to_string_lossy();
            let canonical_str = canonical_home.to_string_lossy();
            if !test_str.starts_with(canonical_str.as_ref())
                && !test_str.starts_with(home_gnx.to_string_lossy().as_ref())
            {
                return Err(PathError::Illegal(format!(
                    "computed index_dir {:?} escapes home_gnx {:?}",
                    index_dir, home_gnx
                )));
            }
        }

        Ok(Self { index_dir, disambiguator })
    }
}

fn hash8(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    hex::encode(&digest[..4])
}

/// Resolve the gnx home directory used for `registry.json` and per-branch
/// index dirs. Tries `$HOME/.gnx` first; if HOME is unset or the directory
/// cannot be created and written to (read-only FS, permission denied, CI
/// sandbox), falls back to `<temp_dir>/gnx-rs-fallback/.gnx`.
///
/// Reads and writes within a single CLI invocation use the same resolved
/// path: a project indexed in fallback mode is queryable from the same
/// environment without extra flags.
pub fn resolve_home_gnx() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".gnx");
        if probe_writable(&candidate) {
            return candidate;
        }
    }
    std::env::temp_dir().join("gnx-rs-fallback").join(".gnx")
}

fn probe_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".gnx-write-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_writable_true_for_normal_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(probe_writable(tmp.path()));
        // probe file should be cleaned up
        assert!(!tmp.path().join(".gnx-write-probe").exists());
    }

    #[cfg(unix)]
    #[test]
    fn probe_writable_false_for_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_mode(0o500); // read+exec, no write
        std::fs::set_permissions(&ro, perms).unwrap();
        assert!(!probe_writable(&ro));
        // restore perms so tempdir cleanup works
        let mut p = std::fs::metadata(&ro).unwrap().permissions();
        p.set_mode(0o700);
        std::fs::set_permissions(&ro, p).unwrap();
    }

    #[test]
    fn probe_writable_false_when_path_is_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!probe_writable(&file));
    }
}
