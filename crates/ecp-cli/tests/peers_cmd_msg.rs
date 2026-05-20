use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_ecp").into()
}

#[test]
fn say_to_targeted_peer_writes_to_their_inbox() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");

    // Set up peerA with a meta.json (alive_peers needs it)
    let pa = sessions.join("peerA");
    std::fs::create_dir_all(&pa).unwrap();
    let meta = format!(
        r#"{{"version":1,"session_id":"peerA","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
        pid = std::process::id()
    );
    std::fs::write(pa.join("meta.json"), meta).unwrap();

    let out = Command::new(bin())
        .args([
            "peers",
            "say",
            "hello peerA",
            "--to",
            "peerA",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me")
        .env("CLAUDE_CODE_SESSION_ID", "me")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let inbox = pa.join("inbox.jsonl");
    let body = std::fs::read_to_string(&inbox).unwrap();
    assert!(
        body.contains("\"body\":\"hello peerA\""),
        "peerA inbox missing message: {body}"
    );

    // Sender's msg.log should also record this with direction=sent
    let me_log = sessions.join("me/msg.log");
    let me_body = std::fs::read_to_string(&me_log).unwrap();
    assert!(
        me_body.contains("\"direction\":\"sent\""),
        "sender msg.log missing sent record"
    );
    assert!(me_body.contains("\"body\":\"hello peerA\""));
}

#[test]
fn broadcast_writes_to_all_alive_peer_inboxes() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    for sid in ["peerA", "peerB"] {
        let s = sessions.join(sid);
        std::fs::create_dir_all(&s).unwrap();
        let meta = format!(
            r#"{{"version":1,"session_id":"{sid}","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
            pid = std::process::id()
        );
        std::fs::write(s.join("meta.json"), meta).unwrap();
    }
    let out = Command::new(bin())
        .args([
            "peers",
            "say",
            "hello team",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me_bcast")
        .env("CLAUDE_CODE_SESSION_ID", "me_bcast")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    for sid in ["peerA", "peerB"] {
        let inbox = sessions.join(sid).join("inbox.jsonl");
        let body = std::fs::read_to_string(&inbox).unwrap();
        assert!(
            body.contains("\"body\":\"hello team\""),
            "{sid} inbox missing broadcast: {body}"
        );
    }
}

#[test]
fn inbox_subcommand_reads_without_draining() {
    let dir = tempdir().unwrap();
    let me_dir = dir.path().join("sessions/me_inbox_test");
    std::fs::create_dir_all(&me_dir).unwrap();
    let entry = r#"{"type":"message","ts":"t","msg_id":"m_x","from":"who","to":null,"reply_to":null,"body":"persist-me"}"#;
    std::fs::write(me_dir.join("inbox.jsonl"), format!("{entry}\n")).unwrap();

    let out = Command::new(bin())
        .args(["peers", "inbox", "--repo", dir.path().to_str().unwrap()])
        .env("ECP_SESSION_ID", "me_inbox_test")
        .env("CLAUDE_CODE_SESSION_ID", "me_inbox_test")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read_to_string(me_dir.join("inbox.jsonl")).unwrap();
    assert!(
        after.contains("persist-me"),
        "inbox subcommand drained the file (should be non-destructive): {after}"
    );
}
