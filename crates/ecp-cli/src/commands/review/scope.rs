use ecp_core::EcpError;
use std::path::{Path, PathBuf};

pub fn resolve(args: &super::ReviewArgs, repo_dir: &Path) -> Result<Vec<PathBuf>, EcpError> {
    // Ahead of the `--files` early return, because `--since` survives it:
    // `aggregate` still uses the value to build a per-file added-line filter.
    // Validating only on the path that reads it here left
    // `review --files x.rs --since <option-shaped>` collapsing the rejection
    // into "the diff was deferred", so the caller got an analysis missing the
    // filter it asked for and a footnote instead of an error.
    if let Some(r) = &args.since {
        crate::git::safe_exec::reject_option_like_rev(r)?;
    }
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
/// A `spec` that names the worktree side re-reads stat-dirty files through
/// their clean filter, so the filter overrides guard it.
fn diff_name_only(repo_dir: &Path, spec: &str) -> Result<Vec<PathBuf>, EcpError> {
    let overrides =
        crate::git::status::status_overrides(repo_dir, &[]).map_err(|e| EcpError::GitDiff {
            reason: format!("filter drivers: {e}"),
        })?;
    let mut git = crate::git::safe_exec::git();
    overrides.apply(&mut git);
    let out = git
        .args(["diff"])
        .args(crate::git::safe_exec::DIFF_HARDENING)
        .args([spec, "--name-only"])
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
/// It reads no file content, so no filter runs.
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
            include: vec![],
        };
        // resolve returns the explicit list without touching git.
        let v = resolve(&args, &PathBuf::from(".")).unwrap();
        assert_eq!(v, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }

    /// `git diff HEAD --name-only` re-reads a stat-dirty file through its
    /// clean filter. The control runs the same command without overrides.
    #[cfg(unix)]
    #[test]
    fn test_diff_name_only_head_filter_driver_never_runs() {
        use crate::git::safe_exec;
        use std::fs::{self, File, FileTimes};
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        fn git(repo: &Path, args: &[&str]) {
            let out = safe_exec::git()
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("f.txt"), "a\n").unwrap();
        fs::write(repo.join(".gitattributes"), "f.txt filter=evil\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "one"]);
        let marker = root.join("filter-ran");
        let script = root.join("filter.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf ran > '{}'\ncat\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &repo,
            &["config", "filter.evil.clean", script.to_str().unwrap()],
        );
        git(&repo, &["config", "filter.evil.required", "true"]);
        // Same size as the committed `a\n`, with an mtime the index has not
        // recorded, so git must compare content through the filter.
        let change = |ago: u64| {
            fs::write(repo.join("f.txt"), "b\n").unwrap();
            File::options()
                .write(true)
                .open(repo.join("f.txt"))
                .unwrap()
                .set_times(
                    FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(ago)),
                )
                .unwrap();
        };

        change(1000);
        let _ = safe_exec::git()
            .args(["diff"])
            .args(safe_exec::DIFF_HARDENING)
            .args(["HEAD", "--name-only"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            marker.exists(),
            "control: the diff without overrides did not run the driver"
        );
        fs::remove_file(&marker).unwrap();

        change(1001);
        assert_eq!(
            diff_name_only(&repo, "HEAD").unwrap(),
            vec![PathBuf::from("f.txt")]
        );
        assert!(!marker.exists(), "the guarded diff ran the driver");
    }
}
