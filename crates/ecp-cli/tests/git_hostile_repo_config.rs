//! A scanned repository is untrusted input. Its `.git/config` and
//! `.gitattributes` can name programs for git to run, and a user-supplied
//! revision can carry an option that makes git write a file.
//!
//! Each test builds the hostile repo, runs the real code path, and asserts on
//! the side effect the attack is after — the marker file, the victim file —
//! not on the flag list. Asserting the flag list would pass for any spelling
//! of the flag, including one git ignores.

use ecp_cli::git::{DiffScope, GitDiffProvider, ShellGitProvider};
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo with two commits, so `diff HEAD~1 HEAD` has content to render.
fn repo_with_two_commits(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "."]);
    git(dir, &["config", "user.email", "t@example.invalid"]);
    git(dir, &["config", "user.name", "t"]);
    fs::write(dir.join("f.txt"), "a\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "one"]);
    fs::write(dir.join("f.txt"), "b\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "two"]);
}

/// A script that records the fact it ran, then behaves like the tool git
/// expected. `marker` is absolute so the payload writes it wherever git runs.
fn payload_script(path: &Path, marker: &Path) {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf 'executed' > '{}'\ncat \"$1\" 2>/dev/null\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn diff_external_from_repo_config_does_not_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let marker = tmp.path().join("external-ran");
    let script = tmp.path().join("payload.sh");
    payload_script(&script, &marker);
    git(
        &repo,
        &["config", "diff.external", script.to_str().unwrap()],
    );

    let _ = ShellGitProvider.diff(&repo, &DiffScope::Compare("HEAD~1".into()));

    assert!(
        !marker.exists(),
        "diff.external from the scanned repo's own config executed: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[cfg(unix)]
#[test]
fn textconv_from_repo_config_does_not_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let marker = tmp.path().join("textconv-ran");
    let script = tmp.path().join("payload.sh");
    payload_script(&script, &marker);
    fs::write(repo.join(".gitattributes"), "f.txt diff=evil\n").unwrap();
    git(
        &repo,
        &["config", "diff.evil.textconv", script.to_str().unwrap()],
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "attrs"]);
    fs::write(repo.join("f.txt"), "c\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "three"]);

    let _ = ShellGitProvider.diff(&repo, &DiffScope::Compare("HEAD~1".into()));

    assert!(
        !marker.exists(),
        "a textconv driver named by the scanned repo executed: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

#[test]
fn option_shaped_baseline_is_rejected_before_git_sees_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let victim = tmp.path().join("victim.txt");
    fs::write(&victim, "original content").unwrap();

    let baseline = format!("--output={}", victim.display());
    let result = ShellGitProvider.diff(&repo, &DiffScope::Compare(baseline));

    assert!(
        result.is_err(),
        "an option-shaped revision must be rejected, not passed to git"
    );
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "original content",
        "git wrote its output over the victim file"
    );
}

#[test]
fn ordinary_revisions_still_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let diffs = ShellGitProvider
        .diff(&repo, &DiffScope::Compare("HEAD~1".into()))
        .expect("a plain revision must still work");
    assert_eq!(
        diffs
            .iter()
            .map(|d| d.file_path.as_str())
            .collect::<Vec<_>>(),
        vec!["f.txt"],
        "the hardening must not change what a normal diff reports"
    );
}

#[cfg(unix)]
#[test]
fn repo_supplied_hooks_do_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    // No config needed for this one: an executable in `.git/hooks/` is the
    // whole attack, and ecp checks out and stashes on the user's behalf.
    let marker = tmp.path().join("hook-ran");
    let hook = repo.join(".git/hooks/post-checkout");
    payload_script(&hook, &marker);

    let out = ecp_cli::git::safe_exec::git()
        .args(["checkout", "--detach", "HEAD~1"])
        .current_dir(&repo)
        .output()
        .expect("git checkout");
    assert!(
        out.status.success(),
        "checkout itself must still work: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !marker.exists(),
        "the scanned repo's post-checkout hook ran: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
}

/// The hardening lives in per-invocation flags, not in a `-c` that empties
/// `diff.external`: git reads an empty external driver as a command to run and
/// dies with `cannot run :` on every ordinary diff. This asserts the plain
/// path still works, so nobody reintroduces that `-c` and breaks every caller
/// who renders a diff without [`DIFF_HARDENING`].
#[test]
fn a_bare_hardened_git_still_renders_an_ordinary_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let out = ecp_cli::git::safe_exec::git()
        .args(["diff", "HEAD~1", "HEAD"])
        .current_dir(&repo)
        .output()
        .expect("git diff");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a plain diff through safe_exec must succeed, got: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("+b"),
        "expected the diff body, got stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `review --since` reaches git through two paths: the file list, and the
/// per-file added-line filter. `--files` short-circuits the first, so
/// validating only there left the second reachable — and it is a write, not
/// just a dropped filter. Against the unfixed binary this exact invocation
/// created the victim file and reported `status: clean`.
///
/// The value uses the `--since=<v>` form on purpose: with a space, clap takes
/// `--output=...` for an unknown flag of its own and rejects it before any of
/// this code runs, so the spaced form would pass without proving anything.
#[test]
fn review_rejects_an_option_shaped_since_even_when_files_are_given() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let victim = tmp.path().join("victim");

    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args([
            "review",
            "--repo",
            repo.to_str().unwrap(),
            "--files",
            "f.txt",
            &format!("--since=--output={}", victim.display()),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env_remove("ECP_HOME")
        .output()
        .expect("run ecp review");

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !victim.exists(),
        "git ran with the injected --output and created the file: {text}"
    );
    assert!(
        !out.status.success(),
        "the rejection has to reach the exit status too, or a script reads \
         this run as a clean review: {text}"
    );
    assert!(
        text.contains("not a revision") || text.contains("starts with `-`"),
        "the rejection must name the option-shaped revision: {text}"
    );
}

/// A filter driver is the third way a scanned repository names a program for
/// git to run, and the only one that fires on the commands `ecp diff` uses to
/// move the worktree. `core.hooksPath=/dev/null` does not touch it, and neither
/// `checkout` nor `stash` has a `--no-filters`.
///
/// Run once per way a repository can place the driver, because the first fix
/// enumerated `--config --local` and two of these three are invisible there:
/// `.git/config.worktree` needs `extensions.worktreeConfig`, and `include.path`
/// needs nothing at all. Both executed on checkout when measured.
///
/// `process` is set alongside `smudge` and `clean` on purpose: it takes
/// precedence, so a fix covering only the other two leaves every case red.
///
/// What this cannot show: a driver in the user's *global* config is overridden
/// too, and that is deliberate — a repository shadows a global name with its
/// own value, so an allowlist keyed on the name waves the attack through.
/// Asserting it would mean setting global git config from inside a test binary
/// whose other tests run in the same process, so the guarantee rests on
/// `filter_overrides` passing no scope flag rather than on a case here.
#[cfg(unix)]
#[test]
fn repo_defined_filter_drivers_do_not_run_during_checkout_or_stash() {
    for placement in ["local", "worktree-config", "include-path"] {
        assert_filter_placement_is_neutralised(placement);
    }
}

#[cfg(unix)]
fn assert_filter_placement_is_neutralised(placement: &str) {
    use ecp_cli::commands::diff::git_guard::GitGuard;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    repo_with_two_commits(&repo);

    let marker = tmp.path().join("filter-ran");
    let script = tmp.path().join("filter.sh");
    // A filter reads the blob on stdin and writes the converted bytes to
    // stdout, so the passthrough here is a bare `cat`, not `cat "$1"`.
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'executed' > '{}'\ncat\n",
            marker.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fs::write(repo.join(".gitattributes"), "f.txt filter=evil\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "attrs"]);

    let driver = script.to_str().unwrap();
    let keys = [
        "filter.evil.smudge",
        "filter.evil.clean",
        "filter.evil.process",
    ];
    match placement {
        "local" => {
            for k in keys {
                git(&repo, &["config", k, driver]);
            }
        }
        "worktree-config" => {
            git(&repo, &["config", "extensions.worktreeConfig", "true"]);
            for k in keys {
                git(&repo, &["config", "--worktree", k, driver]);
            }
        }
        "include-path" => {
            let extra = repo.join(".git/extra.cfg");
            fs::write(
                &extra,
                format!(
                    "[filter \"evil\"]\n\tsmudge = {driver}\n\tclean = {driver}\n\tprocess = {driver}\n"
                ),
            )
            .unwrap();
            git(&repo, &["config", "include.path", "./extra.cfg"]);
        }
        other => panic!("unknown placement {other}"),
    }

    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD~1"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // Left dirty so the guard takes its `stash push -u` path as well.
    fs::write(repo.join("f.txt"), "dirty\n").unwrap();

    {
        let _guard = GitGuard::enter(&repo, &base).expect("the guard must still enter");
        // Without this the test would also pass for a guard that checked
        // nothing out, which never reaches a filter at all.
        assert_eq!(
            fs::read_to_string(repo.join("f.txt")).unwrap(),
            "b\n",
            "[{placement}] the guard must check out the requested base"
        );
        assert!(
            !marker.exists(),
            "[{placement}] a filter driver ran during stash/checkout: {}",
            fs::read_to_string(&marker).unwrap_or_default()
        );
    }

    assert!(
        !marker.exists(),
        "[{placement}] a filter driver ran while the guard restored the worktree: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );
    assert_eq!(
        fs::read_to_string(repo.join("f.txt")).unwrap(),
        "dirty\n",
        "[{placement}] the guard must hand the dirty worktree back unchanged"
    );
}
