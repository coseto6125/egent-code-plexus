//! F2: on a warm attach the overlay re-parses every file that differs
//! between the sibling commit and the worktree, so `find` answers what HEAD
//! holds and drops the warm-attach caveat. When the overlay cannot cover the
//! changes (over the file limit, `git diff` failed), the caveat stays and an
//! exact miss is `found: null`, `status: "unverified"`, with `reason` and
//! `next`.
//!
//! Every fixture indexes commit 1, then commits once more without a rebuild:
//! the next query warm-attaches commit 1's graph (`ECP_SKIP_BG_REBUILD`).

use crate::common;

use common::{commit_all, ecp_bin, run_git};
use std::path::Path;
use std::process::{Command, Output};

const WARM_ATTACH_NOTE: &str = "warm-attach from a sibling commit";
const NEXT: &str = "ecp admin index --force --repo .";
const UNCOVERED: [(&str, &str); 1] = [("ECP_WARM_ATTACH_MAX_FILES", "0")];

const SESSION_KEYS: [&str; 5] = [
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];

fn run_ecp(cwd: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    for key in SESSION_KEYS {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("ecp failed to spawn")
}

/// A repo holding `files` at commit 1, indexed.
fn indexed(files: &[(&str, &str)]) -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        std::fs::write(repo.path().join(rel), text).unwrap();
    }
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    commit_all(repo.path(), "init");
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["admin", "index", "--repo", "."],
        &[],
    );
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (repo, home)
}

fn find(repo: &Path, home: &Path, pattern: &str, env: &[(&str, &str)]) -> serde_json::Value {
    let out = run_ecp(
        repo,
        home,
        &["find", pattern, "--all", "--format", "json"],
        env,
    );
    assert!(
        out.status.success(),
        "find {pattern} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("l2.warm-attach"),
        "the fixture must warm-attach: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON value ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn caveat(payload: &serde_json::Value) -> &str {
    payload["result"].as_str().unwrap_or_default()
}

fn files(payload: &serde_json::Value) -> Vec<&str> {
    payload["matches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["file"].as_str())
        .collect()
}

/// Failed before F2: the sibling graph has no `beta`, so `find` answered
/// `found: false` under a "results may be incomplete" caveat.
#[test]
fn test_find_warm_attach_head_adds_symbol_returns_it_without_caveat() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    std::fs::write(
        repo.path().join("lib.rs"),
        "pub fn alpha() {}\npub fn beta() {}\n",
    )
    .unwrap();
    commit_all(repo.path(), "add beta");

    let payload = find(repo.path(), home.path(), "beta", &[]);
    assert_eq!(payload["found"], true, "{payload}");
    assert_eq!(files(&payload), ["lib.rs"], "{payload}");
    assert!(!caveat(&payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

/// A symbol HEAD removed is gone: the miss carries no warm-attach caveat
/// and is no unverified answer.
#[test]
fn test_find_warm_attach_head_removes_symbol_returns_trustworthy_miss() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\npub fn gamma() {}\n")]);
    std::fs::write(repo.path().join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    commit_all(repo.path(), "remove gamma");

    let payload = find(repo.path(), home.path(), "gamma", &[]);
    assert_eq!(payload["found"], false, "{payload}");
    assert_eq!(payload["status"], "success", "{payload}");
    assert!(!caveat(&payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

/// A renamed file answers at its new path only: the old path's nodes are a
/// tombstone, the new path's a fragment.
#[test]
fn test_find_warm_attach_renamed_file_returns_new_path_only() {
    let (repo, home) = indexed(&[
        ("lib.rs", "pub fn alpha() {}\n"),
        ("old.rs", "pub fn moved() {}\n"),
    ]);
    run_git(repo.path(), &["mv", "old.rs", "new.rs"]);
    commit_all(repo.path(), "rename");

    let payload = find(repo.path(), home.path(), "moved", &[]);
    assert_eq!(payload["found"], true, "{payload}");
    assert_eq!(files(&payload), ["new.rs"], "{payload}");
    assert!(!caveat(&payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

#[test]
fn test_find_warm_attach_deleted_file_misses_its_symbol() {
    let (repo, home) = indexed(&[
        ("lib.rs", "pub fn alpha() {}\n"),
        ("gone.rs", "pub fn doomed() {}\n"),
    ]);
    run_git(repo.path(), &["rm", "-q", "gone.rs"]);
    commit_all(repo.path(), "delete");

    let payload = find(repo.path(), home.path(), "doomed", &[]);
    assert_eq!(payload["found"], false, "{payload}");
    assert_eq!(payload["status"], "success", "{payload}");
    assert!(!caveat(&payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

/// Uncommitted edits on top of the sibling diff: the committed change
/// (`beta`), an edit to the same file (`epsilon`) and an untracked file
/// (`delta`) all answer.
#[test]
fn test_find_warm_attach_uncommitted_edits_over_sibling_diff_covers_both() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    std::fs::write(
        repo.path().join("lib.rs"),
        "pub fn alpha() {}\npub fn beta() {}\n",
    )
    .unwrap();
    commit_all(repo.path(), "add beta");
    std::fs::write(
        repo.path().join("lib.rs"),
        "pub fn alpha() {}\npub fn beta() {}\npub fn epsilon() {}\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("other.rs"), "pub fn delta() {}\n").unwrap();

    for (name, file) in [
        ("beta", "lib.rs"),
        ("epsilon", "lib.rs"),
        ("delta", "other.rs"),
    ] {
        let payload = find(repo.path(), home.path(), name, &[]);
        assert_eq!(payload["found"], true, "{name}: {payload}");
        assert_eq!(files(&payload), [file], "{name}: {payload}");
        assert!(
            !caveat(&payload).contains(WARM_ATTACH_NOTE),
            "{name}: {payload}"
        );
    }
}

fn assert_unverified_miss(payload: &serde_json::Value) {
    assert!(payload["found"].is_null(), "found must be null: {payload}");
    assert_eq!(payload["status"], "unverified", "{payload}");
    assert!(
        payload["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "{payload}"
    );
    assert_eq!(payload["next"], NEXT, "{payload}");
    assert!(caveat(payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

fn add_beta_commit(repo: &Path) {
    std::fs::write(repo.join("lib.rs"), "pub fn alpha() {}\npub fn beta() {}\n").unwrap();
    commit_all(repo, "add beta");
}

/// More changed files than the overlay limit: no cover, so the miss is no
/// answer.
#[test]
fn test_find_warm_attach_changed_files_over_limit_returns_unverified_miss() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());

    let payload = find(repo.path(), home.path(), "beta", &UNCOVERED);
    assert_unverified_miss(&payload);
    assert!(
        payload["reason"].as_str().unwrap().contains("limit"),
        "{payload}"
    );
}

/// An uncovered hit keeps its rows and the caveat; only a miss is
/// unverified.
#[test]
fn test_find_warm_attach_uncovered_hit_keeps_caveat_and_found_true() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());

    let payload = find(repo.path(), home.path(), "alpha", &UNCOVERED);
    assert_eq!(payload["found"], true, "{payload}");
    assert_eq!(payload["status"], "success", "{payload}");
    assert!(caveat(&payload).contains(WARM_ATTACH_NOTE), "{payload}");
}

/// A `git` whose `diff` fails and whose other subcommands run the real git.
#[cfg(unix)]
fn failing_diff_git(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("sh failed to spawn");
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    assert!(!real.is_empty(), "no git on PATH");
    let script = dir.join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = diff ]; then echo 'fatal: injected' >&2; exit 128; fi\ndone\nexec '{real}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn test_find_warm_attach_git_diff_fails_returns_unverified_miss() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());
    let bin = tempfile::tempdir().unwrap();
    let path = failing_diff_git(bin.path());

    let payload = find(repo.path(), home.path(), "beta", &[("PATH", &path)]);
    assert_unverified_miss(&payload);
    assert!(
        payload["reason"].as_str().unwrap().contains("git diff"),
        "{payload}"
    );
}

/// The text form of an unverified miss says so on its first line, then the
/// caveat.
#[test]
fn test_find_warm_attach_uncovered_text_miss_says_unverified() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["find", "beta", "--format", "text"],
        &UNCOVERED,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].starts_with("unverified") && lines[0].contains(NEXT),
        "{stdout}"
    );
    assert!(
        lines[1..].iter().any(|l| l.contains(WARM_ATTACH_NOTE)),
        "{stdout}"
    );
}

/// Failed before F2: impact said "No symbol named" as if the name were
/// absent at HEAD.
#[test]
fn test_impact_warm_attach_uncovered_no_symbol_says_unverified() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["impact", "--target", "beta", "--format", "json"],
        &UNCOVERED,
    );
    assert!(!out.status.success(), "the miss stays an error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("No symbol named"), "{stderr}");
    assert!(stderr.contains("unverified"), "{stderr}");
    assert!(stderr.contains(NEXT), "{stderr}");
}

/// A covered warm attach finds the symbol for impact too.
#[test]
fn test_impact_warm_attach_covered_finds_head_symbol() {
    let (repo, home) = indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    add_beta_commit(repo.path());
    let out = run_ecp(
        repo.path(),
        home.path(),
        &["impact", "--target", "beta", "--format", "json"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
