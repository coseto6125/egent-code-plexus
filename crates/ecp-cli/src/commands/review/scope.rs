use ecp_core::EcpError;
use std::path::{Path, PathBuf};

pub fn resolve(args: &super::ReviewArgs, repo_dir: &Path) -> Result<Vec<PathBuf>, EcpError> {
    if let Some(files) = &args.files {
        return Ok(files.iter().map(PathBuf::from).collect());
    }
    match &args.since {
        Some(r) => diff_name_only(repo_dir, &format!("{r}...HEAD")),
        None => {
            let mut tracked = diff_name_only(repo_dir, "HEAD")?;
            tracked.extend(untracked_files(repo_dir)?);
            Ok(tracked)
        }
    }
}

/// Run `git diff <spec> --name-only` and parse the newline-separated output.
fn diff_name_only(repo_dir: &Path, spec: &str) -> Result<Vec<PathBuf>, EcpError> {
    let out = crate::git::safe_exec::git()
        .args(["diff", spec, "--name-only"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::GitDiff {
            reason: format!("spawn failed: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(EcpError::GitDiff {
            reason: format!("git diff --name-only: {}", stderr.trim()),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Run `git ls-files --others --exclude-standard` to list untracked files.
fn untracked_files(repo_dir: &Path) -> Result<Vec<PathBuf>, EcpError> {
    let out = crate::git::safe_exec::git()
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::GitDiff {
            reason: format!("spawn failed: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(EcpError::GitDiff {
            reason: format!("git ls-files: {}", stderr.trim()),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn explicit_files_override_since() {
        let args = super::super::ReviewArgs {
            since: Some("main".into()),
            files: Some(vec!["a.rs".into(), "b.rs".into()]),
            repo: None,
            format: None,
            verdicts: false,
        };
        // resolve returns the explicit list without touching git.
        let v = resolve(&args, &PathBuf::from(".")).unwrap();
        assert_eq!(v, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }
}
