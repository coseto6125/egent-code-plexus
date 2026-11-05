use crate::common::peer_harness::PeerHarness;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    env!("CARGO_BIN_EXE_ecp").into()
}

/// A peer's session dir name: `<sid>~<h8>`, the only shape a peer listing accepts.
fn peer_dir(sid: &str) -> String {
    format!("{sid}~aaaaaaaa")
}

#[test]
fn say_to_targeted_peer_writes_to_their_inbox() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");

    // Set up peerA with a session_meta.json (alive_peers needs it)
    let pa = sessions.join(peer_dir("peerA"));
    std::fs::create_dir_all(&pa).unwrap();
    let meta = format!(
        r#"{{"version":1,"session_id":"peerA","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
        pid = std::process::id()
    );
    std::fs::write(pa.join("session_meta.json"), meta).unwrap();

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
    let me_log = sessions.join(PeerHarness::dir_name("me")).join("msg.log");
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
        let s = sessions.join(peer_dir(sid));
        std::fs::create_dir_all(&s).unwrap();
        let meta = format!(
            r#"{{"version":1,"session_id":"{sid}","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1}}"#,
            pid = std::process::id()
        );
        std::fs::write(s.join("session_meta.json"), meta).unwrap();
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
        let inbox = sessions.join(peer_dir(sid)).join("inbox.jsonl");
        let body = std::fs::read_to_string(&inbox).unwrap();
        assert!(
            body.contains("\"body\":\"hello team\""),
            "{sid} inbox missing broadcast: {body}"
        );
    }
}

fn write_named_session(sessions: &std::path::Path, sid: &str, agent_name: Option<&str>) {
    let s = sessions.join(sid);
    std::fs::create_dir_all(&s).unwrap();
    let name_field = agent_name
        .map(|n| format!(r#","agent_name":"{n}""#))
        .unwrap_or_default();
    let meta = format!(
        r#"{{"version":1,"session_id":"{sid}","pid":{pid},"started_at":"2026-01-01T00:00:00Z","last_touched":"2026-01-01T00:00:00Z","base_sha":"0000000000000000000000000000000000000000","source_worktree":"/tmp","overlay_version":1{name_field}}}"#,
        pid = std::process::id()
    );
    std::fs::write(s.join("session_meta.json"), meta).unwrap();
}

#[test]
fn say_to_agent_name_resolves_to_session_inbox() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, &peer_dir("s-alpha"), Some("rust-parser"));

    let out = Command::new(bin())
        .args([
            "peers",
            "say",
            "hi by name",
            "--to",
            "rust-parser",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me_byname")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body =
        std::fs::read_to_string(sessions.join(peer_dir("s-alpha")).join("inbox.jsonl")).unwrap();
    assert!(
        body.contains("\"body\":\"hi by name\""),
        "named target inbox missing message: {body}"
    );
}

#[test]
fn say_to_ambiguous_name_errors_listing_candidates() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, &peer_dir("s-one"), Some("worker"));
    write_named_session(&sessions, &peer_dir("s-two"), Some("worker"));

    let out = Command::new(bin())
        .args([
            "peers",
            "say",
            "who gets this?",
            "--to",
            "worker",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me_ambig")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "ambiguous name must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ambiguous"), "stderr: {stderr}");
    assert!(
        stderr.contains("s-one") && stderr.contains("s-two"),
        "candidates must be listed: {stderr}"
    );
    assert!(
        !sessions
            .join(peer_dir("s-one"))
            .join("inbox.jsonl")
            .exists()
            && !sessions
                .join(peer_dir("s-two"))
                .join("inbox.jsonl")
                .exists(),
        "no inbox may be written on ambiguity"
    );
}

#[test]
fn say_to_unknown_name_errors_not_found() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    let out = Command::new(bin())
        .args([
            "peers",
            "say",
            "into the void",
            "--to",
            "ghost",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me_ghost")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "unknown target must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no peer session or agent named"),
        "stderr: {stderr}"
    );
}

#[test]
fn peers_name_sets_own_agent_name() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, &PeerHarness::dir_name("me_namer"), None);

    let out = Command::new(bin())
        .args([
            "peers",
            "name",
            "graph-lead",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me_namer")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            sessions
                .join(PeerHarness::dir_name("me_namer"))
                .join("session_meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["agent_name"], "graph-lead");
}

#[test]
fn inbox_subcommand_reads_without_draining() {
    let dir = tempdir().unwrap();
    let me_dir = dir
        .path()
        .join("sessions")
        .join(PeerHarness::dir_name("me_inbox_test"));
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

/// Nothing is pre-created here. Creating `sessions.join(target)` first would
/// put the escaped directory on disk before the command runs, leaving the test
/// unable to say who made it — and for `..` or `.` it would resolve onto a
/// directory the setup itself owns, so "must not exist" could not be asserted
/// uniformly either.
///
/// Instead the whole temp tree is swept for an inbox afterwards. That holds for
/// every target shape, and it is the outcome that matters: the message must not
/// land anywhere, not merely outside one directory the test happened to name.
fn assert_say_rejects_target(root: &std::path::Path, target: &str) {
    let repo = root.join("cache");
    let sessions = repo.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let out = Command::new(bin())
        .args(["peers", "say", "must not arrive", "--to", target, "--repo"])
        .arg(&repo)
        .env("ECP_HOME", root.join("home"))
        .env("ECP_SESSION_ID", "sender")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "invalid target accepted: {target:?}");
    assert!(stderr.contains(&format!("{target:?}")), "stderr: {stderr}");
    assert!(
        stderr.contains("single normal path component"),
        "stderr: {stderr}"
    );

    let stray = find_inbox_files(root);
    assert!(
        stray.is_empty(),
        "target {target:?} produced an inbox: {stray:?}"
    );
    assert!(!sessions.join(PeerHarness::dir_name("sender")).exists());
}

/// Every `inbox.jsonl` or its lock anywhere under `dir`.
fn find_inbox_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_inbox_files(&path));
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("inbox.jsonl"))
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn say_absolute_target_errors_without_writing_outside_cache() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("escaped");
    assert_say_rejects_target(dir.path(), target.to_str().unwrap());
}

#[test]
fn say_parent_traversal_errors_without_writing_outside_cache() {
    for target in ["..", "../../escaped"] {
        let dir = tempdir().unwrap();
        assert_say_rejects_target(dir.path(), target);
    }
}

#[test]
fn say_separator_in_target_errors_without_writing_inbox() {
    for target in ["nested/child", ".", ""] {
        let dir = tempdir().unwrap();
        assert_say_rejects_target(dir.path(), target);
    }
}

#[test]
fn say_windows_target_errors_without_creating_directory() {
    for target in [
        r"nested\child",
        "C:child",
        "C:",
        r"C:\child",
        r"\\server\share",
    ] {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("cache");
        std::fs::create_dir(&repo).unwrap();
        let out = Command::new(bin())
            .args(["peers", "say", "must not arrive", "--to", target, "--repo"])
            .arg(&repo)
            .env("ECP_HOME", dir.path().join("home"))
            .env("ECP_SESSION_ID", "sender")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(stderr.contains(&format!("{target:?}")), "stderr: {stderr}");
        assert!(
            stderr.contains("single normal path component"),
            "stderr: {stderr}"
        );
        assert_eq!(std::fs::read_dir(&repo).unwrap().count(), 0);
    }
}

#[test]
fn session_commands_invalid_session_id_error_without_creating_directory() {
    let commands: &[&[&str]] = &[
        &["peers", "say", "must not arrive"],
        &["peers", "inbox", "--clear"],
        &["peers", "thread", "m_test"],
        &["peers", "name", "worker"],
        &["peers", "log"],
        &["peers", "gc"],
        &["watch", "--status"],
    ];
    for args in commands {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("cache");
        std::fs::create_dir(&repo).unwrap();
        let out = Command::new(bin())
            .args(*args)
            .arg("--repo")
            .arg(&repo)
            .env("ECP_HOME", dir.path().join("home"))
            .env("ECP_SESSION_ID", "../../escaped")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "args: {args:?}");
        assert!(stderr.contains("../../escaped"), "stderr: {stderr}");
        assert!(
            stderr.contains("single normal path component"),
            "stderr: {stderr}"
        );
        assert!(!dir.path().join("escaped").exists());
        assert_eq!(std::fs::read_dir(&repo).unwrap().count(), 0);
    }
}

fn say_to(repo: &std::path::Path, target: &str) -> std::process::Output {
    Command::new(bin())
        .args(["peers", "say", "hi", "--to", target, "--repo"])
        .arg(repo)
        .env("ECP_SESSION_ID", "me_sender")
        .env("CLAUDE_CODE_SESSION_ID", "me_sender")
        .output()
        .expect("spawn")
}

#[test]
fn test_peers_msg_bare_session_id_two_worktrees_reports_ambiguity() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "shared~aaaaaaaa", None);
    write_named_session(&sessions, "shared~bbbbbbbb", None);

    let out = say_to(dir.path(), "shared");

    assert!(!out.status.success(), "ambiguous bare sid was accepted");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("shared~aaaaaaaa") && stderr.contains("shared~bbbbbbbb"),
        "stderr must list both ids: {stderr}"
    );
    assert!(
        !sessions.join("shared~aaaaaaaa/inbox.jsonl").exists()
            && !sessions.join("shared~bbbbbbbb/inbox.jsonl").exists(),
        "an ambiguous target must receive nothing"
    );
}

#[test]
fn test_peers_msg_bare_session_id_one_worktree_delivers_to_it() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "solo~aaaaaaaa", None);
    write_named_session(&sessions, "soloist~cccccccc", None);

    let out = say_to(dir.path(), "solo");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(sessions.join("solo~aaaaaaaa/inbox.jsonl")).unwrap();
    assert!(body.contains("\"body\":\"hi\""), "inbox: {body}");
    assert!(
        !sessions.join("soloist~cccccccc/inbox.jsonl").exists(),
        "a longer session id sharing the prefix is another session"
    );
}

#[test]
fn test_peers_msg_qualified_id_as_printed_by_status_delivers() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "shared~aaaaaaaa", None);
    write_named_session(&sessions, "shared~bbbbbbbb", None);

    let out = say_to(dir.path(), "shared~bbbbbbbb");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sessions.join("shared~bbbbbbbb/inbox.jsonl").exists());
    assert!(!sessions.join("shared~aaaaaaaa/inbox.jsonl").exists());
}

/// Contract: a bare target reaches exactly the `<sid>~<h8>` dirs of that sid.
/// A sid that itself holds `~`, a retired dir and a 0.17.3 bare dir are never
/// delivery targets, and a hex suffix of the wrong length or case is no h8.
#[test]
fn test_resolve_target_session_lookalike_dirs_never_match_bare_target() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "alice~1234abcd", None);
    for lookalike in [
        "alice~worker~1234abcd",
        "alice~1234abcd.dead.1700000000",
        "alice~1234abcd.stale-ff00ff00",
        "alice~1234abc",
        "alice~1234abcde",
        "alice~5678ABCD",
    ] {
        write_named_session(&sessions, lookalike, None);
    }

    let out = say_to(dir.path(), "alice");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sessions.join("alice~1234abcd/inbox.jsonl").exists());
    let inboxes: Vec<_> = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("inbox.jsonl").exists())
        .collect();
    assert_eq!(inboxes.len(), 1, "only the real worktree session receives");
}

#[test]
fn test_resolve_target_session_legacy_bare_dir_alone_is_not_delivered_to() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "v0173sess", None);

    let out = say_to(dir.path(), "v0173sess");

    assert!(
        !out.status.success(),
        "a 0.17.3 bare dir received a message"
    );
    assert!(!sessions.join("v0173sess/inbox.jsonl").exists());
}

#[test]
fn test_resolve_target_session_legacy_bare_dir_beside_live_one_routes_to_live() {
    let dir = tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    write_named_session(&sessions, "alice", None);
    write_named_session(&sessions, "alice~1234abcd", None);

    let out = say_to(dir.path(), "alice");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sessions.join("alice~1234abcd/inbox.jsonl").exists());
    assert!(!sessions.join("alice/inbox.jsonl").exists());
}

#[test]
fn test_resolve_target_session_empty_sessions_dir_reports_not_found() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    let out = say_to(dir.path(), "alice");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no peer session or agent named"));
}

#[test]
fn test_resolve_target_session_missing_sessions_dir_reports_not_found() {
    let dir = tempdir().unwrap();
    let out = say_to(dir.path(), "alice");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no peer session or agent named"));
}
