//! Scratch checkout of the baseline commit for `ecp diff`.
//!
//! The baseline graph is built from a detached `git worktree` of the baseline
//! SHA in a unique temp dir. The user's working tree, index and stash list are
//! never touched, so no path (error, panic, concurrent second `ecp diff`)
//! can lose or reshape their uncommitted work.
//!
//! On drop the worktree is removed with `git worktree remove --force` and the
//! temp dir deleted by the exact path this guard created. A SIGINT skips
//! `Drop`: the temp dir and one prunable `.git/worktrees/<name>` entry stay,
//! and the user's tree is still untouched.
//!
//! Content conversion is what runs a repository's own filter driver, so the
//! tree is written by a `reset --hard` that carries `safe_exec::filter_overrides`
//! read *inside* the new worktree. `worktree add` only registers it
//! (`--no-checkout`): its own checkout runs under the new worktree's config,
//! where an `includeIf "gitdir:**/worktrees/**"` section can define a driver
//! that the main worktree's config never shows.

use crate::git::safe_exec::{self, FilterOverrides};
use ecp_core::EcpError;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct BaselineCheckout {
    repo_dir: PathBuf,
    checkout_root: PathBuf,
    /// `repo_dir` relative to the repository top level, so a diff run from a
    /// subdirectory indexes the same subdirectory of the baseline.
    prefix: PathBuf,
    scratch: TempDir,
    filter_overrides: FilterOverrides,
}

impl BaselineCheckout {
    pub fn create(repo_dir: &Path, target_sha: &str) -> Result<Self, EcpError> {
        let main_overrides = read_filter_overrides(repo_dir)?;
        let scratch = tempfile::Builder::new()
            .prefix("ecp-diff-baseline-")
            .tempdir()
            .map_err(|e| EcpError::Output(format!("create baseline scratch dir: {e}")))?;
        let checkout_root = scratch.path().join("checkout");
        let prefix = show_prefix(repo_dir)?;

        run_git(
            safe_exec::git_with_overrides(repo_dir, &main_overrides)
                .args(["worktree", "add", "--no-checkout", "--detach"])
                .arg(&checkout_root)
                .arg(target_sha),
            &format!("git worktree add {target_sha}"),
        )?;
        // Built before the reset, so a failed reset still removes the
        // registered worktree on drop.
        let mut checkout = Self {
            repo_dir: repo_dir.to_path_buf(),
            checkout_root,
            prefix,
            scratch,
            filter_overrides: main_overrides,
        };
        let linked = read_filter_overrides(&checkout.checkout_root)?;
        checkout.filter_overrides.merge(linked);

        run_git(
            safe_exec::git_with_overrides(&checkout.checkout_root, &checkout.filter_overrides)
                .args(["reset", "--hard", "-q", "--no-recurse-submodules"]),
            "git reset --hard in the baseline worktree",
        )?;
        Ok(checkout)
    }

    /// Directory to index: the baseline counterpart of the user's `repo_dir`.
    pub fn path(&self) -> PathBuf {
        self.checkout_root.join(&self.prefix)
    }

    /// Private temp dir for intermediate files; removed with the checkout.
    pub fn scratch(&self) -> &Path {
        self.scratch.path()
    }
}

impl Drop for BaselineCheckout {
    fn drop(&mut self) {
        let removed = safe_exec::git_with_overrides(&self.repo_dir, &self.filter_overrides)
            .args(["worktree", "remove", "--force"])
            .arg(&self.checkout_root)
            .output();
        match removed {
            Err(e) => eprintln!("BaselineCheckout drop: git worktree remove failed: {e}"),
            Ok(out) if !out.status.success() => eprintln!(
                "BaselineCheckout drop: git worktree remove stderr: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Ok(_) => {}
        }
    }
}

fn read_filter_overrides(dir: &Path) -> Result<FilterOverrides, EcpError> {
    safe_exec::filter_overrides(dir)
        .map_err(|e| EcpError::Output(format!("read filter drivers in {}: {e}", dir.display())))
}

fn run_git(cmd: &mut std::process::Command, what: &str) -> Result<(), EcpError> {
    let out = cmd
        .output()
        .map_err(|e| EcpError::Output(format!("{what} failed to spawn: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn show_prefix(repo_dir: &Path) -> Result<PathBuf, EcpError> {
    let out = safe_exec::git()
        .args(["rev-parse", "--show-prefix"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| EcpError::Output(format!("git rev-parse failed: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "git rev-parse --show-prefix failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // git prints the prefix verbatim plus one newline. A directory name may
    // begin with a space or hold bytes that are not UTF-8, so only that
    // newline comes off.
    let mut prefix = out.stdout;
    if prefix.last() == Some(&b'\n') {
        prefix.pop();
    }
    Ok(path_from_git_bytes(prefix))
}

#[cfg(unix)]
fn path_from_git_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Everything a stash/checkout would disturb, as bytes.
    fn snapshot(repo: &Path) -> Vec<String> {
        vec![
            git(repo, &["status", "--porcelain=v2", "--branch", "-uall"]),
            git(repo, &["diff"]),
            git(repo, &["diff", "--cached"]),
            git(repo, &["stash", "list", "--format=%H %gs"]),
            git(repo, &["rev-parse", "HEAD"]),
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            std::fs::read_to_string(repo.join("untracked.txt")).unwrap(),
        ]
    }

    fn dirty_repo() -> (TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "v1\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "v1"]);
        let baseline = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(repo.join("a.txt"), "v2\n").unwrap();
        git(repo, &["commit", "-qam", "v2"]);
        std::fs::write(repo.join("a.txt"), "stashed\n").unwrap();
        git(repo, &["stash", "push", "-qm", "users-own-stash"]);
        // Staged edit + further unstaged edit on the same file + untracked file.
        std::fs::write(repo.join("a.txt"), "staged\n").unwrap();
        git(repo, &["add", "a.txt"]);
        std::fs::write(repo.join("a.txt"), "dirty on top\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();
        (dir, baseline)
    }

    #[test]
    fn test_create_dirty_tree_leaves_tree_index_and_stash_byte_identical() {
        let (dir, baseline) = dirty_repo();
        let before = snapshot(dir.path());
        {
            let checkout = BaselineCheckout::create(dir.path(), &baseline).unwrap();
            assert_eq!(
                std::fs::read_to_string(checkout.path().join("a.txt")).unwrap(),
                "v1\n"
            );
            assert!(!checkout.path().join("untracked.txt").exists());
            assert_eq!(
                snapshot(dir.path()),
                before,
                "tree disturbed while checkout is live"
            );
        }
        assert_eq!(snapshot(dir.path()), before, "tree disturbed after drop");
    }

    #[test]
    fn test_create_unknown_sha_errors_and_leaves_tree_untouched() {
        let (dir, _) = dirty_repo();
        let before = snapshot(dir.path());
        let bogus = "0123456789abcdef0123456789abcdef01234567";
        assert!(BaselineCheckout::create(dir.path(), bogus).is_err());
        assert_eq!(snapshot(dir.path()), before);
        assert_eq!(git(dir.path(), &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn test_drop_removes_worktree_registration_and_scratch_dir() {
        let (dir, baseline) = dirty_repo();
        let scratch;
        {
            let checkout = BaselineCheckout::create(dir.path(), &baseline).unwrap();
            scratch = checkout.scratch().to_path_buf();
            assert!(git(dir.path(), &["worktree", "list"]).lines().count() == 2);
        }
        assert!(!scratch.exists());
        assert_eq!(git(dir.path(), &["worktree", "list"]).lines().count(), 1);
    }

    #[test]
    fn test_create_two_checkouts_at_once_use_distinct_dirs() {
        let (dir, baseline) = dirty_repo();
        let a = BaselineCheckout::create(dir.path(), &baseline).unwrap();
        let b = BaselineCheckout::create(dir.path(), &baseline).unwrap();
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn test_create_from_subdirectory_indexes_the_matching_baseline_subdirectory() {
        let (dir, baseline) = dirty_repo();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("s.txt"), "x").unwrap();
        // Untracked in the user's tree, absent from the baseline checkout.
        let checkout = BaselineCheckout::create(&sub, &baseline).unwrap();
        assert!(checkout.path().ends_with("checkout/sub"));
    }

    #[test]
    fn test_show_prefix_leading_space_directory_keeps_the_space() {
        let (dir, _) = dirty_repo();
        let sub = dir.path().join(" src");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(show_prefix(&sub).unwrap(), PathBuf::from(" src/"));
    }

    #[test]
    fn test_create_from_leading_space_directory_indexes_that_baseline_directory() {
        let (dir, _) = dirty_repo();
        let sub = dir.path().join(" src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("s.txt"), "in space dir\n").unwrap();
        git(dir.path(), &["add", " src/s.txt"]);
        git(dir.path(), &["commit", "-qm", "space dir"]);
        let head = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_string();
        let checkout = BaselineCheckout::create(&sub, &head).unwrap();
        assert_eq!(
            std::fs::read_to_string(checkout.path().join("s.txt")).unwrap(),
            "in space dir\n"
        );
    }
}
