//! `ecp diff` builds its baseline graph from a scratch checkout that it deletes
//! when the diff ends. The build used to record that checkout as the worktree
//! it came from, so the next diff rejected the slot as built below a root that
//! no longer exists: it indexed the baseline again and left one more
//! generation of that SHA, which gc never retires.

use ecp_core::registry::CommitBuildMeta;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn ecp_diff(home: &Path, wt: &Path, baseline: &str) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["diff", "--section", "symbols", "--baseline", baseline])
        .args(["--format", "json"])
        .current_dir(wt)
        .env("ECP_HOME", home)
        .env("HOME", home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env("ECP_NO_PROGRESS", "1")
        .output()
        .expect("run ecp diff");
    assert!(
        out.status.success(),
        "ecp diff failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The one `commits/` dir under `home`.
fn commits_dir(home: &Path) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(home)
        .unwrap()
        .flatten()
        .map(|e| e.path().join("commits"))
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap()
}

fn commit_dirs(commits: &Path) -> Vec<String> {
    let mut names: Vec<String> = ecp_cli::commit_lookup::CommitDirListing::read(commits)
        .unwrap()
        .dirs
        .into_iter()
        .map(|d| d.name)
        .collect();
    names.sort();
    names
}

/// Pass criterion, fixed before the run: the bug means the second diff prints
/// `l2.built` (or `l2.rebuilt`) and the commits dir gains a generation; the
/// fix means neither, and the baseline slot names the user's repository as
/// its worktree.
#[test]
fn test_diff_second_run_same_baseline_attaches_no_new_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&wt, &["init", "-q"]);
    std::fs::write(wt.join("a.ts"), "function a() { return 1; }\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "one"]);
    let baseline = git(&wt, &["rev-parse", "HEAD"]);
    std::fs::write(wt.join("b.ts"), "function b() { return a(); }\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "two"]);

    let first = ecp_diff(&home, &wt, &baseline);
    assert!(
        String::from_utf8_lossy(&first.stderr).contains("l2.built"),
        "the first diff builds: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let commits = commits_dir(&home);
    let before = commit_dirs(&commits);

    let second = ecp_diff(&home, &wt, &baseline);

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        !stderr.contains("l2.built") && !stderr.contains("l2.rebuilt"),
        "the second diff built again: {stderr}"
    );
    assert_eq!(commit_dirs(&commits), before);
    let baseline_dir = before
        .iter()
        .find(|name| name.ends_with(&baseline))
        .unwrap_or_else(|| panic!("no baseline slot in {before:?}"));
    let meta = CommitBuildMeta::read(&commits.join(baseline_dir).join("meta.json")).unwrap();
    let recorded = dunce::canonicalize(&meta.built_from_worktree).unwrap();
    assert_eq!(recorded, dunce::canonicalize(&wt).unwrap());
}
