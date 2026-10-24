//! `ecp admin install-hook` without `--claude-code` used to write a git
//! `reference-transaction` hook. `ecp hook-handle` now does nothing, so the
//! command refuses, writes nothing, and names the command that removes a hook
//! an older release left. `--claude-code` keeps working.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn run_ecp(args: &[&str], home: &Path, cwd: &Path) -> Output {
    Command::new(ecp_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .expect("ecp spawn failed")
}

/// A repository whose path holds a space, plus a separate HOME.
fn repo_and_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("my repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    (tmp, repo, home)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn test_run_without_claude_code_refuses_and_writes_nothing() {
    let (_tmp, repo, home) = repo_and_home();
    let hooks = repo.join(".git/hooks");
    let cases: [&[&str]; 3] = [&[], &["--force"], &["--no-chain"]];
    for flags in cases {
        let before = listing(&hooks);
        let mut args = vec!["admin", "install-hook"];
        args.extend_from_slice(flags);
        let out = run_ecp(&args, &home, &repo);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "{flags:?}: install-hook must exit non-zero; stderr={stderr}"
        );
        assert!(
            stderr.contains("ecp admin uninstall-hook"),
            "{flags:?}: the refusal must name the removal command; stderr={stderr}"
        );
        assert!(
            !hooks.join("reference-transaction").exists(),
            "{flags:?}: no hook may be written"
        );
        assert_eq!(listing(&hooks), before, "{flags:?}: hooks dir changed");
    }
}

#[test]
fn test_run_without_claude_code_leaves_an_existing_foreign_hook_byte_identical() {
    let (_tmp, repo, home) = repo_and_home();
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let foreign = "#!/bin/sh\nexec some-other-tool \"$@\"\n";
    std::fs::write(&hook, foreign).unwrap();
    let cases: [&[&str]; 3] = [&["--force"], &["--no-chain"], &[]];
    for flags in cases {
        let mut args = vec!["admin", "install-hook"];
        args.extend_from_slice(flags);
        assert!(!run_ecp(&args, &home, &repo).status.success());
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), foreign);
        assert!(!hook.with_extension("chained-prev").exists());
    }
    assert!(
        !listing(hook.parent().unwrap())
            .iter()
            .any(|n| n.starts_with("reference-transaction.bak.")),
        "no backup may be made"
    );
}

#[test]
fn test_run_with_claude_code_installs_settings_entries() {
    let (tmp, repo, home) = repo_and_home();
    let settings = tmp.path().join("settings.json");
    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "install-hook",
            "--claude-code",
            "--events",
            "session-start",
            "--settings-path",
        ])
        .arg(&settings)
        .current_dir(&repo)
        .env("HOME", &home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(&settings).unwrap();
    assert!(body.contains("session-start"), "settings: {body}");
    assert!(!repo.join(".git/hooks/reference-transaction").exists());
}
