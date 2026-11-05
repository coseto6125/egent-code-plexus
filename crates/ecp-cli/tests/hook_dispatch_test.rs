//! Verifies the `ecp hook <event> --claude-code` subcommand parses
//! and dispatches without panic on a minimal stdin envelope. Per-event
//! behaviour is exercised in dedicated test files (T2-T7).

use std::io::Write;
use std::process::{Command, Stdio};

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

#[test]
fn pre_tool_use_no_match_returns_empty_stdout() {
    let mut child = Command::new(ecp_bin())
        .args(["hook", "pre-tool-use", "--claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"cwd": "/tmp", "tool_name": "Bash", "tool_input": {"command": "ls"}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "expected empty stdout for no-op, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn missing_host_flag_errors() {
    let out = Command::new(ecp_bin())
        .args(["hook", "pre-tool-use"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit when --claude-code is missing"
    );
}

// ── Heartbeat: which session dir a hook event beats ─────────────────────────
//
// Contract: every hook event beats the qualified session dir
// (`<sid>~<worktree key>`) of the session the hook names (env, then the
// envelope's `session_id`), and also of the worktree a Bash `cd` or an
// Edit/Write `file_path` points into. A session dir that never beats expires
// from `peers status` after 120 minutes.

mod heartbeat {
    use super::ecp_bin;
    use ecp_cli::repo_identity::repo_dir_name_for_cwd;
    use ecp_cli::session::resolver::session_dir_name;
    use ecp_core::session::SessionMeta;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, SystemTime};

    const SID: &str = "hb-session";

    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        repo: PathBuf,
        sibling: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("repo");
        let sibling = tmp.path().join("sibling");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("lib.py"), "def f():\n    pass\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                sibling.to_str().unwrap(),
            ],
        );
        let fx = Fixture {
            _tmp: tmp,
            home,
            repo,
            sibling,
        };
        for worktree in [&fx.repo, &fx.sibling] {
            fx.enroll(worktree);
        }
        fx
    }

    impl Fixture {
        fn session_dir(&self, worktree: &Path) -> PathBuf {
            self.home
                .join(".ecp")
                .join(repo_dir_name_for_cwd(worktree).unwrap())
                .join("sessions")
                .join(session_dir_name(SID, worktree))
        }

        /// A session meta last touched long ago, its file mtime old enough to
        /// pass the once-a-minute throttle.
        fn enroll(&self, worktree: &Path) {
            let dir = self.session_dir(worktree);
            std::fs::create_dir_all(&dir).unwrap();
            let meta = SessionMeta {
                version: 1,
                session_id: dir.file_name().unwrap().to_string_lossy().into_owned(),
                pid: None,
                started_at: "2026-01-01T00:00:00Z".into(),
                last_touched: "2026-01-01T00:00:00Z".into(),
                base_sha: "0".repeat(40),
                source_worktree: worktree.to_string_lossy().into_owned(),
                overlay_version: 0,
                watcher_pid: None,
                last_drained_offset: 0,
                agent_name: None,
            };
            let path = dir.join("session_meta.json");
            SessionMeta::write_atomic(&path, &meta).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(600))
                .unwrap();
        }

        fn beaten(&self, worktree: &Path) -> bool {
            let meta =
                SessionMeta::read(&self.session_dir(worktree).join("session_meta.json")).unwrap();
            meta.last_touched != "2026-01-01T00:00:00Z"
        }

        /// Fires `pre-tool-use` with no session env var at all.
        fn fire(&self, envelope: serde_json::Value) {
            let mut child = Command::new(ecp_bin())
                .args(["hook", "pre-tool-use", "--claude-code"])
                .env("HOME", &self.home)
                .env("ECP_SKIP_BG_REBUILD", "1")
                .env_remove("ECP_HOME")
                .env_remove("ECP_SESSION_ID")
                .env_remove("CLAUDE_CODE_SESSION_ID")
                .env_remove("CODEX_SESSION_ID")
                .env_remove("CODEX_THREAD_ID")
                .env_remove("GEMINI_CLI_SESSION_ID")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(envelope.to_string().as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn test_hook_heartbeat_session_id_from_input_when_env_absent_beats_qualified_session() {
        let fx = fixture();
        fx.fire(serde_json::json!({
            "session_id": SID, "cwd": fx.repo,
            "tool_name": "Bash", "tool_input": {"command": "ls"},
        }));
        assert!(fx.beaten(&fx.repo), "the cwd worktree's session beats");
        assert!(
            !fx.beaten(&fx.sibling),
            "a sibling worktree the call never touched keeps its old stamp"
        );
    }

    #[test]
    fn test_hook_heartbeat_no_session_id_anywhere_beats_nothing() {
        let fx = fixture();
        fx.fire(serde_json::json!({
            "cwd": fx.repo, "tool_name": "Bash", "tool_input": {"command": "ls"},
        }));
        assert!(!fx.beaten(&fx.repo) && !fx.beaten(&fx.sibling));
    }

    #[test]
    fn test_hook_heartbeat_bash_cd_into_sibling_worktree_beats_that_worktree() {
        let fx = fixture();
        let command = format!("cd {} && grep -rn needle .", fx.sibling.display());
        fx.fire(serde_json::json!({
            "session_id": SID, "cwd": fx.repo,
            "tool_name": "Bash", "tool_input": {"command": command},
        }));
        assert!(fx.beaten(&fx.sibling), "the cd target's session beats");
        assert!(fx.beaten(&fx.repo), "so does the cwd's");
    }

    #[test]
    fn test_hook_heartbeat_edit_file_in_sibling_worktree_beats_that_worktree() {
        let fx = fixture();
        fx.fire(serde_json::json!({
            "session_id": SID, "cwd": fx.repo, "tool_name": "Edit",
            "tool_input": {
                "file_path": fx.sibling.join("lib.py"),
                "old_string": "pass", "new_string": "return 1",
            },
        }));
        assert!(fx.beaten(&fx.sibling), "the edited file's worktree beats");
    }
}
