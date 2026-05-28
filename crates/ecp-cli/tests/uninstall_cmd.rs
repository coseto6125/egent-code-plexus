//! Integration tests for `ecp uninstall`.
//!
//! NB: every integration test here must pass `--dry-run` or `--agent`. A bare
//! `ecp uninstall` now deletes the running binary as its last step — which, for
//! `CARGO_BIN_EXE_ecp`, is the test harness's own copy. Exercise the real
//! deletion via the `remove_self_binary_at` unit tests (tmpdir), never the CLI.

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

#[test]
fn test_remove_git_hook_at_leaves_bak_timestamp_files_untouched() {
    // `ecp admin install-hook --force` (or --no-chain) renames the pre-ecp
    // hook to `reference-transaction.bak.<ts>`. Uninstall must NOT delete
    // those files — they're the user's pre-ecp backup. It surfaces them in
    // stdout instead so the user can decide.
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");
    let bak1 = dir.path().join("reference-transaction.bak.1700000000");
    let bak2 = dir.path().join("reference-transaction.bak.1800000000");

    std::fs::write(
        &hook_path,
        "#!/bin/sh\n# ecp-managed reference-transaction hook\nexec \"/usr/local/bin/ecp\" hook-handle \"$@\"\n",
    )
    .unwrap();
    std::fs::write(&bak1, "#!/bin/sh\nexec old-tool \"$@\"\n").unwrap();
    std::fs::write(&bak2, "#!/bin/sh\nexec other-tool \"$@\"\n").unwrap();

    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();

    // Active hook removed.
    assert!(!hook_path.exists(), "ecp-managed hook should be gone");
    // Backups preserved verbatim.
    assert!(bak1.exists(), "bak.1700000000 must NOT be auto-deleted");
    assert!(bak2.exists(), "bak.1800000000 must NOT be auto-deleted");
    assert_eq!(
        std::fs::read_to_string(&bak1).unwrap(),
        "#!/bin/sh\nexec old-tool \"$@\"\n",
        "backup content untouched"
    );
}

#[test]
fn test_remove_git_hook_at_reports_backups_even_when_no_hook_present() {
    // `ecp uninstall` is sometimes a no-op for the hook itself (already gone)
    // but the user may still have ancient backups from a long-past install.
    // Surface them so the manual cleanup path doesn't depend on the hook
    // file existing right now.
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");
    let bak = dir.path().join("reference-transaction.bak.1700000000");
    std::fs::write(&bak, "old").unwrap();

    // Hook itself doesn't exist — this is the "not installed" branch.
    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();
    assert!(bak.exists(), "backup still kept on the missing-hook branch");
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
fn test_uninstall_agent_flag_filters_to_one_agent() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--agent", "codex", "--dry-run"])
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
        "claude steps must be absent when --agent codex"
    );
    assert!(
        !stdout.contains("gemini"),
        "gemini steps must be absent when --agent codex"
    );
    // With --agent set, ecp-cache, git-hook and self-binary are suppressed too.
    assert!(
        !stdout.contains("ecp-cache"),
        "ecp-cache must be suppressed when --agent is set"
    );
    assert!(
        !stdout.contains("git-hook"),
        "git-hook must be suppressed when --agent is set"
    );
    assert!(
        !stdout.contains("self-binary"),
        "self-binary must be suppressed when --agent is set"
    );
}

#[test]
fn test_uninstall_invalid_agent_errors() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--agent", "unknown-agent", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "invalid --agent should cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown agent") || stderr.contains("unknown-agent"),
        "error message should mention the unknown agent, got: {stderr}"
    );
}

#[test]
fn test_uninstall_old_host_flag_is_rejected() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--host", "codex", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "renamed-away --host flag should be a clap error"
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
        stdout.contains("--agent")
            && stdout.contains("--dry-run")
            && stdout.contains("--keep-cache"),
        "help must show all three flags, got: {stdout}"
    );
}

// ─── self-delete binary tests ───────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn test_remove_self_binary_at_deletes_the_file() {
    let dir = TempDir::new().unwrap();
    let fake_bin = dir.path().join("ecp");
    std::fs::write(&fake_bin, b"#!/bin/sh\necho fake ecp\n").unwrap();

    let outcome = ecp_cli::commands::uninstall::remove_self_binary_at(&fake_bin).unwrap();

    assert!(
        matches!(
            outcome,
            ecp_cli::commands::uninstall::SelfDeleteOutcome::Deleted
        ),
        "unix should delete synchronously"
    );
    assert!(!fake_bin.exists(), "binary should be unlinked on unix");
}

#[cfg(windows)]
#[test]
fn test_remove_self_binary_at_schedules_on_windows() {
    let dir = TempDir::new().unwrap();
    let fake_bin = dir.path().join("ecp.exe");
    std::fs::write(&fake_bin, b"fake").unwrap();

    let outcome = ecp_cli::commands::uninstall::remove_self_binary_at(&fake_bin).unwrap();

    assert!(
        matches!(
            outcome,
            ecp_cli::commands::uninstall::SelfDeleteOutcome::Scheduled
        ),
        "windows should schedule a delayed delete, not delete in-process"
    );
}

#[test]
fn test_remove_self_binary_at_missing_is_skip() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does-not-exist");

    let outcome = ecp_cli::commands::uninstall::remove_self_binary_at(&missing).unwrap();
    assert!(
        matches!(
            outcome,
            ecp_cli::commands::uninstall::SelfDeleteOutcome::Skipped
        ),
        "a non-existent binary path is a graceful skip"
    );
}
