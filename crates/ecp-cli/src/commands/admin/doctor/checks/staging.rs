//! Old-style build dirs (`<base>.building/`) that an ecp from before the SHA
//! lock left in `~/.ecp/<repo>/commits/`. No current ecp writes, reads or
//! removes one: an older ecp still running may own it. Its lock is not
//! probed either: an older ecp that tries that lock while a probe holds it
//! fails its build after 5 s. Doctor reports each one with the days since
//! its last write, and the operator removes it once every older ecp has
//! exited.

use crate::commands::admin::doctor::CheckResult;
use crate::commit_lookup::{legacy_staging_last_write, CommitDirListing, StagingShape};
use ecp_core::registry::resolve_home_ecp;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) fn check() -> Vec<CheckResult> {
    check_in(&resolve_home_ecp(), SystemTime::now())
}

fn check_in(home: &Path, now: SystemTime) -> Vec<CheckResult> {
    let mut leftovers: Vec<PathBuf> = Vec::new();
    if let Ok(repos) = std::fs::read_dir(home) {
        for repo in repos.flatten() {
            let commits = repo.path().join("commits");
            let Ok(listing) = CommitDirListing::read(&commits) else {
                continue;
            };
            leftovers.extend(
                listing
                    .staging
                    .into_iter()
                    .filter(|s| s.shape == StagingShape::Legacy)
                    .map(|s| commits.join(s.name)),
            );
        }
    }
    if leftovers.is_empty() {
        return vec![CheckResult::ok("staging", "no legacy build dirs")];
    }
    leftovers.sort();
    leftovers
        .iter()
        .map(|dir| {
            CheckResult::warn(
                "staging:legacy",
                format!("{} age={}", dir.display(), age(dir, now)),
            )
            .with_remediation(format!(
                "after every older ecp has exited: {}",
                remove_command(dir)
            ))
        })
        .collect()
}

/// Days since the newest write to the dir, its `meta.json` or its
/// `graph.bin` (the same age gc judges it by); `?` when none can be read or
/// the time is in the future.
fn age(dir: &Path, now: SystemTime) -> String {
    legacy_staging_last_write(dir)
        .and_then(|newest| now.duration_since(newest).ok())
        .map_or_else(|| "?".to_string(), |d| format!("{}d", d.as_secs() / 86_400))
}

fn remove_command(dir: &Path) -> String {
    #[cfg(windows)]
    {
        format!(
            "Remove-Item -Recurse -LiteralPath '{}'",
            dir.display().to_string().replace('\'', "''")
        )
    }
    #[cfg(not(windows))]
    {
        format!("rm -r {}", crate::background::shell_quote(dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::admin::doctor::CheckStatus;
    use std::time::Duration;

    const SHA: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

    fn legacy(home: &Path) -> PathBuf {
        let dir = home
            .join("repo")
            .join("commits")
            .join(format!("branch_main__{SHA}.building"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("graph.bin"), b"half").unwrap();
        dir
    }

    #[test]
    fn test_check_in_no_leftovers_ok() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo").join("commits")).unwrap();
        std::fs::write(tmp.path().join("registry.json"), b"{}").unwrap();
        let results = check_in(tmp.path(), SystemTime::now());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, CheckStatus::Ok);
    }

    /// F1: reported with path and age, and left on disk.
    #[test]
    fn test_check_in_legacy_dir_reported_not_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = legacy(tmp.path());
        let later = SystemTime::now() + Duration::from_secs(3 * 86_400 + 60);
        let results = check_in(tmp.path(), later);
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(
            (r.name.as_str(), r.status),
            ("staging:legacy", CheckStatus::Warn)
        );
        assert_eq!(
            r.message,
            format!("{} age=3d", dir.display()),
            "path and age as key=value fields"
        );
        let hint = r.remediation.as_deref().unwrap();
        assert!(
            hint.starts_with("after every older ecp has exited: "),
            "{hint}"
        );
        assert!(hint.contains(&dir.display().to_string()), "{hint}");
        assert!(dir.join("graph.bin").exists());
        assert!(
            !dir.join(".build.lock").exists(),
            "the probe creates nothing"
        );
    }

    /// The legacy lock is never probed: an older builder that holds it is
    /// reported by age alone.
    #[test]
    fn test_check_in_legacy_lock_held_reported_by_age_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = legacy(tmp.path());
        let held =
            ecp_core::registry::FileLock::acquire_exclusive(&dir.join(".build.lock")).unwrap();
        let results = check_in(tmp.path(), SystemTime::now());
        assert_eq!(results[0].message, format!("{} age=0d", dir.display()));
        drop(held);
    }

    /// New-style staging is gc's (`sweep_abandoned_staging`), not doctor's.
    #[test]
    fn test_check_in_new_style_staging_not_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let dir =
            tmp.path()
                .join("repo")
                .join("commits")
                .join(crate::commit_lookup::staging_dir_name(
                    &format!("branch_main__{SHA}"),
                    42,
                    0,
                ));
        std::fs::create_dir_all(dir).unwrap();
        let results = check_in(tmp.path(), SystemTime::now());
        assert_eq!(results[0].status, CheckStatus::Ok);
    }
}
