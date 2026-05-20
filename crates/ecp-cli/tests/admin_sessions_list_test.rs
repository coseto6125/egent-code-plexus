use std::process::Command;

fn ecp_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("ecp")
}

#[test]
fn admin_sessions_list_runs_with_empty_home() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn admin_sessions_list_json_emits_empty_array() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed.is_array());
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}

#[test]
fn admin_sessions_list_shows_pure_reference_state() {
    let home = tempfile::tempdir().unwrap();
    let repo_root = home.path().join(".ecp/myrepo__deadbeef");
    let commit_dir =
        repo_root.join("commits/branch_main__abc123def456789012345678901234567890abcd");
    std::fs::create_dir_all(&commit_dir).unwrap();
    let cm = ecp_core::registry::CommitBuildMeta {
        version: 1,
        sha: "abc123def456789012345678901234567890abcd".into(),
        source_type: ecp_core::registry::SourceType::Branch,
        source_id: Some("main".into()),
        built_from_worktree: "/tmp/wt".into(),
        built_at: "2026-05-17T10:00:00Z".into(),
        parent_sha: None,
        node_count: 0,
        embedding_status: ecp_core::registry::EmbeddingStatus::None,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
    };
    ecp_core::registry::CommitBuildMeta::write_atomic(&commit_dir.join("meta.json"), &cm).unwrap();

    let sd = repo_root.join("sessions/sid_a");
    std::fs::create_dir_all(&sd).unwrap();
    let sm = ecp_core::session::SessionMeta {
        version: 1,
        session_id: "sid_a".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: "abc123def456789012345678901234567890abcd".into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    ecp_core::session::SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    ecp_core::session::DirtyFiles::write_atomic(
        &sd.join("dirty_files.json"),
        &ecp_core::session::DirtyFiles::empty(),
    )
    .unwrap();

    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["admin", "sessions", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["state"]["kind"], "pure_reference");
}
