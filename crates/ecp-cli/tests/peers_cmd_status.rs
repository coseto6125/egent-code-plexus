use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_ecp").into()
}

#[test]
fn peers_status_empty_repo_prints_no_peers() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    let out = Command::new(bin())
        .args(["peers", "status", "--repo", dir.path().to_str().unwrap()])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no peers") || stdout.is_empty() || stdout.contains("[]"),
        "unexpected status output: {stdout}"
    );
}

fn write_session(root: &std::path::Path, id: &str, agent_name: Option<&str>) {
    use ecp_core::session::SessionMeta;
    let dir = root.join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let meta = SessionMeta {
        version: 1,
        session_id: id.into(),
        pid: Some(std::process::id()),
        started_at: now.clone(),
        last_touched: now,
        base_sha: "0".repeat(40),
        source_worktree: String::new(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: agent_name.map(str::to_string),
    };
    SessionMeta::write_atomic(&dir.join("session_meta.json"), &meta).unwrap();
}

#[test]
fn peers_status_shows_agent_name_and_dash_when_absent() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-named", Some("rust-parser"));
    write_session(dir.path(), "s-anon", None);

    let text = Command::new(bin())
        .args(["peers", "status", "--repo", dir.path().to_str().unwrap()])
        .env("ECP_SESSION_ID", "me-status")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        stdout.contains("session=s-named\tname=rust-parser"),
        "named peer missing name column: {stdout}"
    );
    assert!(
        stdout.contains("session=s-anon\tname=-"),
        "anon peer missing dash placeholder: {stdout}"
    );

    let json = Command::new(bin())
        .args([
            "peers",
            "status",
            "--format",
            "json",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-status")
        .output()
        .expect("spawn");
    let rows: serde_json::Value = serde_json::from_slice(&json.stdout).expect("status json parses");
    let by_id = |id: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["session_id"] == id)
            .unwrap_or_else(|| panic!("row {id} missing: {rows}"))
            .clone()
    };
    assert_eq!(by_id("s-named")["agent_name"], "rust-parser");
    assert_eq!(by_id("s-anon")["agent_name"], serde_json::Value::Null);
}

#[test]
fn peers_log_empty_prints_no_messages() {
    let dir = tempdir().unwrap();
    let out = Command::new(bin())
        .args(["peers", "log", "--repo", dir.path().to_str().unwrap()])
        .env("CLAUDE_CODE_SESSION_ID", "test_log_session")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no messages") || stdout.is_empty());
}
