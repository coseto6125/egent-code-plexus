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

// Telemetry showed 14/33 (42%) uninstall invocations failing with
// "invalid argument: unknown agent 'unknown-agent'" (an automated caller
// passing a stale variable). The value stays a hard error — a typo like
// `--agent claud` exiting 0 while leaving claude installed would be a silent
// no-op — but it now fails at the clap layer, listing the allowed values.
#[test]
fn test_uninstall_unknown_agent_fails_at_argparse_listing_values() {
    let out = Command::new(ecp_bin())
        .args(["uninstall", "--agent", "unknown-agent", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "unknown --agent must fail, not silently no-op"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("claude") && stderr.contains("codex") && stderr.contains("gemini"),
        "error should list allowed values, got: {stderr}"
    );
}

// A second run on an already-clean install (no agent files present) must
// also succeed — idempotent uninstall is load-bearing for automated workflows
// that call `ecp uninstall --agent <x>` regardless of current install state.
#[test]
fn test_uninstall_agent_on_clean_install_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    // Empty HOME: no claude/codex/gemini integration files exist.
    for &agent in &["claude", "codex", "gemini"] {
        let out = Command::new(ecp_bin())
            .args(["uninstall", "--agent", agent, "--keep-cache"])
            .env("HOME", tmp.path())
            .env("ECP_HOME", tmp.path().join(".ecp"))
            // Suppress PATH lookups for claude/gemini CLIs so we don't spawn
            // actual system binaries; the test only cares about exit code.
            .env("PATH", tmp.path())
            .output()
            .unwrap();

        assert!(
            out.status.success(),
            "uninstall --agent {agent} on clean install must exit 0, got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
    // The schedule must also run: a wait that exits early leaves the file.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while fake_bin.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(
        !fake_bin.exists(),
        "the scheduled delete must remove the file"
    );
}

/// cmd expands `%NAME%` even inside quotes, so a path segment shaped like an
/// environment variable must reach `del` unexpanded.
#[cfg(windows)]
#[test]
fn test_remove_self_binary_at_percent_segment_deletes_the_real_path() {
    let dir = TempDir::new().unwrap();
    let odd = dir.path().join("a%USERNAME%b");
    std::fs::create_dir(&odd).unwrap();
    let fake_bin = odd.join("ecp.exe");
    std::fs::write(&fake_bin, b"fake").unwrap();

    ecp_cli::commands::uninstall::remove_self_binary_at(&fake_bin).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while fake_bin.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(
        !fake_bin.exists(),
        "the scheduled delete must remove the file under a %-shaped directory"
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

// ─── hook ownership + `ecp admin uninstall-hook` without --claude-code ──────

const ECP_HOOK: &str =
    "#!/bin/sh\n# ecp-managed reference-transaction hook\nexec \"/usr/local/bin/ecp\" hook-handle \"$@\"\n";

#[test]
fn test_is_ecp_reference_transaction_hook_every_release_form_is_owned() {
    use ecp_cli::commands::uninstall::is_ecp_reference_transaction_hook as owned;
    for name in ["gnx", "cgn", "ecp"] {
        let body = format!(
            "#!/bin/sh\n# {name}-managed reference-transaction hook\nexec \"/home/u/bin/{name}\" hook-handle \"$@\"\n"
        );
        assert!(owned(&body), "{name} release hook not recognised");
    }
    let chained = "#!/bin/sh\n# ecp-managed reference-transaction hook\n/repo/.git/hooks/reference-transaction.chained-prev \"$@\" || exit $?\nexec \"C:\\Users\\u\\ecp.exe\" hook-handle \"$@\"\r\n";
    assert!(owned(chained));
    assert!(owned(
        "#!/bin/sh\nexec \"/opt/my dir/ecp\" hook-handle \"$@\"\n"
    ));
}

#[test]
fn test_is_ecp_reference_transaction_hook_foreign_script_mentioning_hook_handle_is_not_owned() {
    use ecp_cli::commands::uninstall::is_ecp_reference_transaction_hook as owned;
    for body in [
        "#!/bin/sh\nother-tool hook-handle \"$@\"\n",
        "#!/bin/sh\n# runs ecp hook-handle later\nexec lefthook run reference-transaction \"$@\"\n",
        "#!/bin/sh\nexec \"\" hook-handle \"$@\"\n",
        "",
    ] {
        assert!(!owned(body), "foreign hook claimed: {body:?}");
    }
}

#[test]
fn test_remove_git_hook_at_foreign_hook_mentioning_hook_handle_left_alone() {
    let dir = TempDir::new().unwrap();
    let hook_path = dir.path().join("reference-transaction");
    let foreign = "#!/bin/sh\nother-tool hook-handle \"$@\"\n";
    std::fs::write(&hook_path, foreign).unwrap();
    ecp_cli::commands::uninstall::remove_git_hook_at(&hook_path).unwrap();
    assert_eq!(std::fs::read_to_string(&hook_path).unwrap(), foreign);
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository at a path with a space, its hooks dir and a separate HOME.
fn hook_repo() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("my repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    (tmp, repo, home)
}

fn uninstall_hook(cwd: &std::path::Path, home: &std::path::Path) -> std::process::Output {
    Command::new(ecp_bin())
        .args(["admin", "uninstall-hook"])
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap()
}

fn assert_ok(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "uninstall-hook failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_uninstall_hook_without_claude_code_removes_ecp_hook_and_second_run_is_noop() {
    let (_tmp, repo, home) = hook_repo();
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, ECP_HOOK).unwrap();

    assert_ok(&uninstall_hook(&repo, &home));
    assert!(!hook.exists(), "ecp hook must be removed");

    let second = uninstall_hook(&repo, &home);
    assert_ok(&second);
    assert!(!hook.exists());
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("not installed"),
        "second run must report nothing to remove: {}",
        String::from_utf8_lossy(&second.stdout)
    );
}

#[test]
fn test_uninstall_hook_without_claude_code_restores_chained_hook() {
    let (_tmp, repo, home) = hook_repo();
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, ECP_HOOK).unwrap();
    let previous = "#!/bin/sh\nexec old-hook \"$@\"\n";
    std::fs::write(hook.with_extension("chained-prev"), previous).unwrap();

    assert_ok(&uninstall_hook(&repo, &home));
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), previous);
    assert!(!hook.with_extension("chained-prev").exists());

    assert_ok(&uninstall_hook(&repo, &home));
    assert_eq!(
        std::fs::read_to_string(&hook).unwrap(),
        previous,
        "a second run must leave the restored hook alone"
    );
}

#[test]
fn test_uninstall_hook_without_claude_code_keeps_foreign_hook() {
    let (_tmp, repo, home) = hook_repo();
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let foreign = "#!/bin/sh\nother-tool hook-handle \"$@\"\n";
    std::fs::write(&hook, foreign).unwrap();

    assert_ok(&uninstall_hook(&repo, &home));
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), foreign);
}

#[test]
fn test_uninstall_hook_relative_core_hooks_path_from_linked_worktree_removes_both() {
    let (tmp, repo, home) = hook_repo();
    git_in(&repo, &["config", "core.hooksPath", "my hooks"]);
    let configured = repo.join("my hooks/reference-transaction");
    std::fs::create_dir_all(configured.parent().unwrap()).unwrap();
    std::fs::write(&configured, ECP_HOOK).unwrap();
    let legacy = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, ECP_HOOK).unwrap();

    // A relative core.hooksPath resolves against the worktree git runs the
    // hook in, so the linked worktree's own `my hooks/` dir is the one in use.
    let linked = tmp.path().join("linked wt");
    git_in(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    let linked_hook = linked.join("my hooks/reference-transaction");
    std::fs::create_dir_all(linked_hook.parent().unwrap()).unwrap();
    std::fs::write(&linked_hook, ECP_HOOK).unwrap();

    assert_ok(&uninstall_hook(&linked, &home));
    assert!(
        !linked_hook.exists(),
        "hook in the effective hooks dir stays"
    );
    assert!(!legacy.exists(), "hook in <common dir>/hooks stays");
    assert!(
        configured.exists(),
        "the main worktree's hooks dir is not the linked worktree's to clean"
    );
}

#[test]
fn test_uninstall_hook_events_without_claude_code_is_rejected() {
    let (_tmp, repo, home) = hook_repo();
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, ECP_HOOK).unwrap();
    let out = Command::new(ecp_bin())
        .args(["admin", "uninstall-hook", "--events", "session-start"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(hook.exists(), "a rejected call must not remove the hook");
}
