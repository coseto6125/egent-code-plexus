//! SessionStart hook: template render + worktree detection.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// Run hook with optional HOME override so the subprocess resolves
/// `~/.ecp/registry.json` against a fake home.
fn run_session_start(envelope: &str, home: Option<&std::path::Path>) -> std::process::Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(["hook", "session-start", "--claude-code"]);
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn no_index_present_yields_empty_output() {
    let tmp = TempDir::new().unwrap();
    // Empty HOME → no registry → cwd has no entry → no-op.
    let fake_home = tmp.path().join("home");
    std::fs::create_dir_all(&fake_home).unwrap();
    let envelope = serde_json::json!({ "cwd": tmp.path() }).to_string();
    let out = run_session_start(&envelope, Some(&fake_home));
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "no registry entry and not a worktree → no-op expected"
    );
}

#[test]
#[ignore = "fixture uses v1 <repo>/<branch>/meta.json (BranchMeta) layout; rewrite to v2 commits/<dirname>/meta.json (CommitBuildMeta) needed"]
fn template_placeholders_get_rendered_when_meta_present() {
    let tmp = TempDir::new().unwrap();
    let fake_home = tmp.path().join("home");
    let home_ecp = fake_home.join(".ecp");
    let repo = tmp.path().join("repo");
    let index_dir = home_ecp.join("alpha").join("main");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::write(
        index_dir.join("meta.json"),
        r#"{"indexed_at":"2026-05-16T00:00:00Z","node_count":1234,"worktree_path":"/x","remote_url":"","schema_version":1}"#,
    )
    .unwrap();
    let claude_dir = repo.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("ecp-rules.md"),
        "stats: {{stats.nodes}} symbols",
    )
    .unwrap();

    let registry = serde_json::json!({
        "version": 1,
        "repos": [{
            "name": "alpha",
            "remote_url": "",
            "worktree_path": repo.to_string_lossy(),
            "index_dir_root": home_ecp.join("alpha").to_string_lossy(),
            "branches": [{
                "name": "main",
                "index_dir": index_dir.to_string_lossy(),
                "indexed_at": "2026-05-16T00:00:00Z",
                "node_count": 1234u32,
                "delta_size": 0u64
            }],
            "groups": []
        }],
        "groups": []
    });
    std::fs::write(
        home_ecp.join("registry.json"),
        serde_json::to_string(&registry).unwrap(),
    )
    .unwrap();

    let envelope = format!(r#"{{"cwd": "{}"}}"#, repo.display());
    let out = run_session_start(&envelope, Some(&fake_home));
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.contains("1234 symbols"),
        "rendered output should substitute {{{{stats.nodes}}}}: got {body}"
    );
    assert!(body.contains("SessionStart"));
}
