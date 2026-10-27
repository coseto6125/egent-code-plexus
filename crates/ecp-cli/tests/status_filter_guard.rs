//! `git status` re-reads a tracked file through its clean filter whenever the
//! file is racily clean, changed in metadata only, or changed in content. ecp
//! runs a status on every query, inside repositories the user did not write,
//! so a `.gitattributes` line naming a driver the repository's config defines
//! would run that program on every query.
//!
//! Each case first runs the status ecp used to run — hardened, without filter
//! overrides — and requires the planted driver to fire, so a fixture that
//! never reaches the driver cannot pass. It then runs the real entry point and
//! asserts on the marker the driver writes and on the dirty set, since a
//! status that merely exits 0 proves nothing.
#![cfg(unix)]

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use ecp_cli::git::diff_parser::DiffHunk;
use ecp_cli::git::{safe_exec, status, DiffScope, FileDiff, GitDiffProvider, ShellGitProvider};

#[derive(Clone, Copy, Debug)]
enum Change {
    RacilyClean,
    MetadataOnly,
    Content,
}

#[derive(Clone, Copy, Debug)]
enum Entry {
    /// The freshness gate every query runs.
    PorcelainAll,
    /// The clean-tree check before a build.
    Plain,
}

/// The setting that names the driver's program. `process` takes precedence
/// over `clean`, so each is planted on its own.
const DRIVER_KEYS: [&str; 2] = ["clean", "process"];

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    marker: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> Output {
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
        // The fixture writes the repository's config; a redirected
        // `git config` would write it to the redirect target instead.
        .env_remove("GIT_CONFIG")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A committed `f.txt` whose attributes name `evil`, with the driver's `key`
/// and `required=true` planted after the commit, so the blob holds the raw
/// bytes and an inert filter leaves the file clean.
fn fixture(key: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let repo = root.join("repo with space");
    let home = root.join("home");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("f.txt"), "a\n").unwrap();
    fs::write(repo.join(".gitattributes"), "f.txt filter=evil\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);

    let marker = root.join("filter-ran");
    let script = root.join("filter.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'executed' > '{}'\ncat\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo,
        &[
            "config",
            &format!("filter.evil.{key}"),
            script.to_str().unwrap(),
        ],
    );
    git(&repo, &["config", "filter.evil.required", "true"]);
    Fixture {
        _tmp: tmp,
        repo,
        home,
        marker,
    }
}

fn set_mtime(path: &Path, at: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(at))
        .unwrap();
}

/// A past time no earlier call returned, so each run meets a stat the index
/// has not recorded.
fn fresh_past() -> SystemTime {
    static TICK: AtomicU64 = AtomicU64::new(0);
    SystemTime::now() - Duration::from_secs(1000 + TICK.fetch_add(1, Ordering::Relaxed))
}

/// Puts `f.txt` in the state `change` names, relative to the index as it is
/// now. Re-applied before every run, because a status that verifies a file
/// records its stat and the next run would trust it.
fn apply(f: &Fixture, change: Change) {
    let file = f.repo.join("f.txt");
    match change {
        // The entry's recorded mtime is not older than the index file, so git
        // must compare content even though the stat matches.
        Change::RacilyClean => {
            let mtime = fs::metadata(&file).unwrap().modified().unwrap();
            set_mtime(&f.repo.join(".git/index"), mtime - Duration::from_secs(100));
        }
        Change::MetadataOnly => set_mtime(&file, fresh_past()),
        // Same size as the committed `a\n`: a size change alone marks the file
        // modified, and git would never read it through the driver.
        Change::Content => {
            fs::write(&file, "b\n").unwrap();
            set_mtime(&file, fresh_past());
        }
    }
}

fn expected_dirty(change: Change) -> &'static [u8] {
    match change {
        Change::RacilyClean | Change::MetadataOnly => b"",
        Change::Content => b" M f.txt\0",
    }
}

fn assert_driver_never_runs(change: Change) {
    for key in DRIVER_KEYS {
        for entry in [Entry::PorcelainAll, Entry::Plain] {
            let f = fixture(key);
            let case = format!("[{key} {change:?} {entry:?}]");

            apply(&f, change);
            let _ = safe_exec::git()
                .args(["status", "--porcelain", "-z", "--untracked-files=all"])
                .current_dir(&f.repo)
                .output()
                .unwrap();
            assert!(
                f.marker.exists(),
                "{case} control: the status without overrides did not run the driver"
            );
            fs::remove_file(&f.marker).unwrap();

            apply(&f, change);
            let out = match entry {
                Entry::PorcelainAll => status::porcelain_all(&f.repo, &f.home),
                Entry::Plain => status::plain(&f.repo),
            }
            .unwrap();
            assert!(
                !f.marker.exists(),
                "{case} the guarded status ran the repository's driver"
            );
            assert!(
                out.status.success(),
                "{case} {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(expected_dirty(change)),
                "{case} dirty set"
            );
        }
    }
}

#[test]
fn test_porcelain_all_racily_clean_filtered_file_driver_never_runs() {
    assert_driver_never_runs(Change::RacilyClean);
}

#[test]
fn test_porcelain_all_metadata_only_change_driver_never_runs() {
    assert_driver_never_runs(Change::MetadataOnly);
}

#[test]
fn test_porcelain_all_content_change_driver_never_runs_and_reports_file() {
    assert_driver_never_runs(Change::Content);
}

/// Runs `test` again in a child test process with `GIT_CONFIG=/dev/null`.
/// The variable redirects `git config` only (git-config(1) ENVIRONMENT), so a
/// probe that inherits it lists no driver while the status still reads the
/// repository's config. The child keeps the variable out of the other tests
/// of this binary. Returns true in the parent.
fn rerun_with_git_config_redirected(test: &str) -> bool {
    if std::env::var_os("ECP_GIT_CONFIG_REDIRECT_CHILD").is_some() {
        return false;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--test-threads=1", "--nocapture"])
        .env("ECP_GIT_CONFIG_REDIRECT_CHILD", "1")
        .env("GIT_CONFIG", "/dev/null")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "child run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    true
}

#[test]
fn test_porcelain_all_git_config_env_redirect_driver_never_runs() {
    if rerun_with_git_config_redirected(
        "test_porcelain_all_git_config_env_redirect_driver_never_runs",
    ) {
        return;
    }
    assert_driver_never_runs(Change::Content);
}

#[test]
fn test_shell_git_provider_diff_git_config_env_redirect_driver_never_runs() {
    if rerun_with_git_config_redirected(
        "test_shell_git_provider_diff_git_config_env_redirect_driver_never_runs",
    ) {
        return;
    }
    test_shell_git_provider_diff_worktree_scopes_driver_never_runs();
}

/// The diff `ShellGitProvider` runs for `scope`, hardened, without filter
/// overrides: the command ecp ran before.
fn unguarded_diff(repo: &Path, scope: &DiffScope) {
    let rev = match scope {
        DiffScope::Unstaged => None,
        DiffScope::Staged => Some("--staged"),
        DiffScope::All => Some("HEAD"),
        DiffScope::Compare(base) => Some(base.as_str()),
    };
    let _ = safe_exec::git()
        .arg("diff")
        .args(safe_exec::DIFF_HARDENING)
        .args(rev)
        .arg("-U0")
        .current_dir(repo)
        .output()
        .unwrap();
}

/// A diff against the worktree re-reads a stat-dirty file through its clean
/// filter, as status does.
#[test]
fn test_shell_git_provider_diff_worktree_scopes_driver_never_runs() {
    let scopes = [
        DiffScope::Unstaged,
        DiffScope::All,
        DiffScope::Compare("HEAD".into()),
    ];
    for key in DRIVER_KEYS {
        for scope in &scopes {
            let f = fixture(key);
            let case = format!("[{key} {scope:?}]");

            apply(&f, Change::Content);
            unguarded_diff(&f.repo, scope);
            assert!(
                f.marker.exists(),
                "{case} control: the diff without overrides did not run the driver"
            );
            fs::remove_file(&f.marker).unwrap();

            apply(&f, Change::Content);
            let got = ShellGitProvider.diff(&f.repo, scope).unwrap();
            assert!(
                !f.marker.exists(),
                "{case} the guarded diff ran the repository's driver"
            );
            assert_eq!(
                got,
                vec![FileDiff {
                    file_path: "f.txt".into(),
                    hunks: vec![DiffHunk {
                        start_line: 1,
                        end_line: 1,
                    }],
                }],
                "{case}"
            );
        }
    }
}

/// `--staged` compares the index with HEAD, so it takes no overrides: even
/// the unguarded command reads no worktree file.
#[test]
fn test_shell_git_provider_diff_staged_reads_no_worktree_file() {
    for key in DRIVER_KEYS {
        let f = fixture(key);
        apply(&f, Change::Content);
        unguarded_diff(&f.repo, &DiffScope::Staged);
        assert!(!f.marker.exists(), "[{key}] the staged diff ran the driver");
        apply(&f, Change::Content);
        assert_eq!(
            ShellGitProvider.diff(&f.repo, &DiffScope::Staged).unwrap(),
            Vec::<FileDiff>::new()
        );
        assert!(!f.marker.exists(), "[{key}] the staged diff ran the driver");
    }
}
