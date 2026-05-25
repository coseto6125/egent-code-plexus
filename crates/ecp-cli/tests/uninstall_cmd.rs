//! Integration tests for `ecp uninstall`.

use std::process::Command;
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

// ─── git hook removal tests (gap A) ─────────────────────────────────────────

#[test]
fn test_remove_git_hook_at_ecp_managed_removes_file() {
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");

    std::fs::write(
        &hook_path,
        "#!/bin/sh\n# ecp-managed reference-transaction hook\nexec \"/usr/local/bin/ecp\" hook-handle \"$@\"\n",
    )
    .unwrap();

    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();

    assert!(!hook_path.exists(), "ecp-managed hook should be deleted");
}

#[test]
fn test_remove_git_hook_at_chained_restores_previous() {
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");
    let chained = dir.path().join("reference-transaction.chained-prev");

    std::fs::write(
        &hook_path,
        "#!/bin/sh\n# ecp-managed reference-transaction hook\nexec \"/usr/local/bin/ecp\" hook-handle \"$@\"\n",
    )
    .unwrap();
    std::fs::write(&chained, "#!/bin/sh\nexec old-hook \"$@\"\n").unwrap();

    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();

    assert!(hook_path.exists(), "hook_path should be restored");
    assert!(!chained.exists(), "chained-prev should be consumed");
    let restored = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        restored.contains("old-hook"),
        "restored content should be the previous hook"
    );
}

#[test]
fn test_remove_git_hook_at_foreign_hook_left_alone() {
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");

    std::fs::write(&hook_path, "#!/bin/sh\nexec some-other-tool \"$@\"\n").unwrap();

    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();

    assert!(hook_path.exists(), "foreign hook must not be removed");
    let body = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        body.contains("some-other-tool"),
        "content must be untouched"
    );
}

#[test]
fn test_remove_git_hook_at_missing_hook_is_noop() {
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");

    // Must not error when hook does not exist.
    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();
}

// ─── CLI surface tests ────────────────────────────────────────────────────────

#[test]
fn test_uninstall_dry_run_lists_without_deleting() {
    let tmp = TempDir::new().unwrap();
    // Write a minimal ~/.ecp structure to the temp dir.
    let ecp_home = tmp.path().join(".ecp");
    std::fs::create_dir_all(ecp_home.join("repo-abc")).unwrap();
    std::fs::write(ecp_home.join("registry.json"), "{}").unwrap();

    let out = Command::new(ecp_bin())
        .args(["uninstall", "--dry-run", "--keep-cache"])
        .env("ECP_HOME", &ecp_home)
        .output()
        .unwrap();

    // Command must succeed.
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run flag should appear in output"
    );
    // The ecp-home directory should remain untouched.
    assert!(ecp_home.exists(), "ecp home must not be deleted in dry-run");
}

#[test]
fn test_uninstall_host_flag_filters_to_one_host() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--host", "codex", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Codex steps appear.
    assert!(stdout.contains("codex"), "codex steps should appear");
    // Claude/Gemini steps must NOT appear (filtered out).
    assert!(
        !stdout.contains("claude"),
        "claude steps must be absent when --host codex"
    );
    assert!(
        !stdout.contains("gemini"),
        "gemini steps must be absent when --host codex"
    );
    // With --host set, ecp-cache and git-hook are suppressed too.
    assert!(
        !stdout.contains("ecp-cache"),
        "ecp-cache must be suppressed when --host is set"
    );
    assert!(
        !stdout.contains("git-hook"),
        "git-hook must be suppressed when --host is set"
    );
}

#[test]
fn test_uninstall_invalid_host_errors() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--host", "unknown-host", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "invalid --host should cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown host") || stderr.contains("unknown-host"),
        "error message should mention the unknown host, got: {stderr}"
    );
}

#[test]
fn test_uninstall_help_is_visible() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--help"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--host")
            && stdout.contains("--dry-run")
            && stdout.contains("--keep-cache"),
        "help must show all three flags, got: {stdout}"
    );
}
