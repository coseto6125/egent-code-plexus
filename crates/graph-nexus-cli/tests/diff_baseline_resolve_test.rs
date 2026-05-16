//! Verify `gnx diff` CLI surface: required args, section enum, baseline rejection.

use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn diff_requires_section_and_baseline() {
    let output = Command::new(gnx_bin())
        .args(["diff"])
        .output()
        .expect("run gnx diff");
    assert!(!output.status.success(), "diff without args must reject");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--section") || stderr.contains("section"),
        "missing-section hint expected, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("--baseline") || stderr.contains("baseline"),
        "missing-baseline hint expected, got stderr: {stderr}"
    );
}

#[test]
fn diff_help_lists_section_choices() {
    let output = Command::new(gnx_bin())
        .args(["diff", "--help"])
        .output()
        .expect("run gnx diff --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for word in ["bindings", "routes", "contracts", "all"] {
        assert!(
            stdout.contains(word),
            "expected `{word}` in --help possible values, got: {stdout}"
        );
    }
}

#[test]
fn diff_baseline_invalid_ref_errors_with_hint() {
    let output = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", "definitely-no-such-ref"])
        .output()
        .expect("run gnx diff");
    assert!(!output.status.success(), "invalid ref must error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot resolve") || stderr.contains("not found")
            || stderr.contains("unknown") || stderr.contains("baseline"),
        "expected unresolvable-ref hint, got: {stderr}"
    );
}

#[test]
fn diff_baseline_pr_form_calls_gh() {
    // Skip when gh is not installed.
    let gh_check = Command::new("gh").arg("--version").output();
    if gh_check.is_err() || !gh_check.unwrap().status.success() {
        eprintln!("skipping: gh CLI not installed");
        return;
    }
    // Use a clearly non-existent PR; gnx should surface a clean error.
    let output = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", "PR/9999999"])
        .output()
        .expect("run gnx diff");
    assert!(!output.status.success(), "non-existent PR must error");
}

#[test]
fn git_guard_restores_branch_on_drop() {
    use std::env;

    // Capture current branch HEAD ref.
    let before = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(env::current_dir().unwrap())
        .output()
        .expect("git symbolic-ref");
    let before_branch = String::from_utf8_lossy(&before.stdout).trim().to_string();
    if before_branch.is_empty() {
        eprintln!("skipping: HEAD is detached");
        return;
    }

    let baseline_sha = {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse")
            .stdout;
        String::from_utf8_lossy(&out).trim().to_string()
    };

    let _ = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", &baseline_sha])
        .output();

    let after = Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .expect("git symbolic-ref");
    let after_branch = String::from_utf8_lossy(&after.stdout).trim().to_string();
    assert_eq!(
        before_branch, after_branch,
        "branch must be restored after diff"
    );
}

#[test]
fn diff_baseline_short_name_warns_on_remote_divergence() {
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    // git init + commit on main
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::write(repo.join("a.txt"), "hello").unwrap();
    let _ = Command::new("git").args(["add", "-A"]).current_dir(repo).output();
    let _ = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "main commit"])
        .current_dir(repo)
        .output();

    // Create a fake `origin/main` ref pointing at a DIFFERENT commit by:
    //   - branching off, committing, then writing the resulting SHA into
    //     refs/remotes/origin/main directly.
    let _ = Command::new("git")
        .args(["checkout", "-q", "-b", "tmp"])
        .current_dir(repo)
        .output();
    std::fs::write(repo.join("b.txt"), "world").unwrap();
    let _ = Command::new("git").args(["add", "-A"]).current_dir(repo).output();
    let _ = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "fake remote commit"])
        .current_dir(repo)
        .output();
    let remote_sha_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    let remote_sha = String::from_utf8_lossy(&remote_sha_out.stdout).trim().to_string();

    // Switch back to main and forge refs/remotes/origin/main.
    let _ = Command::new("git").args(["checkout", "-q", "main"]).current_dir(repo).output();
    let _ = Command::new("git")
        .args(["update-ref", "refs/remotes/origin/main", &remote_sha])
        .current_dir(repo)
        .output();

    // Run gnx diff with --baseline main; expect warning on stderr.
    let output = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", "main"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run gnx diff");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local `main`") && stderr.contains("origin/main") && stderr.contains("differs"),
        "expected divergence warning in stderr, got: {stderr}"
    );
}

#[test]
fn diff_baseline_qualified_ref_no_warning() {
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    let _ = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(repo).output();
    std::fs::write(repo.join("a.txt"), "hello").unwrap();
    let _ = Command::new("git").args(["add", "-A"]).current_dir(repo).output();
    let _ = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "init"])
        .current_dir(repo)
        .output();

    let head_sha = String::from_utf8_lossy(
        &Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();

    // Pass full SHA — should NOT trigger divergence check.
    let output = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", &head_sha])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run gnx diff");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("differs from"),
        "qualified ref must not emit divergence warning, got: {stderr}"
    );
}

#[test]
fn diff_baseline_short_name_no_remote_emits_note() {
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();

    // git init + 1 commit on main, but NO origin remote configured.
    let _ = Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(repo).output();
    std::fs::write(repo.join("a.txt"), "hello").unwrap();
    let _ = Command::new("git").args(["add", "-A"]).current_dir(repo).output();
    let _ = Command::new("git").args([
        "-c","user.email=t@t","-c","user.name=t",
        "commit","-q","-m","init"
    ]).current_dir(repo).output();

    // Run gnx diff --baseline main. No origin remote → expect skip-note on stderr.
    let output = Command::new(env!("CARGO_BIN_EXE_gnx"))
        .args(["diff", "--section", "bindings", "--baseline", "main"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("run gnx diff");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("note:") && stderr.contains("origin/main") && stderr.contains("skipped"),
        "expected missing-remote note in stderr, got: {stderr}"
    );
}
