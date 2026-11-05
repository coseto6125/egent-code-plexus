//! End-to-end coverage for the `ecp peers` feature after the
//! session_meta.json filename fix. Validates the data plane only
//! (alive_peers enumeration + inbox round-trip + status format
//! classification); the inotify watcher daemon path is exercised by
//! the existing common/peer_harness.rs spawning suite.

use crate::common::peer_harness::PeerHarness;
use chrono::Utc;
use ecp_core::peer::registry::alive_peers;
use ecp_core::session::SessionMeta;
use std::path::Path;
use tempfile::tempdir;

/// A peer's session dir name: `<sid>~<h8>`, the only shape a peer listing accepts.
fn peer_dir(sid: &str) -> String {
    format!("{sid}~aaaaaaaa")
}

fn write_session(repo_root: &Path, sid: &str, watcher_pid: Option<u32>) {
    write_session_in(repo_root, &peer_dir(sid), sid, watcher_pid);
}

/// The session `sid` as the binary run from this test's cwd names its own dir.
fn write_own_session(repo_root: &Path, sid: &str) {
    write_session_in(repo_root, &PeerHarness::dir_name(sid), sid, None);
}

fn write_session_in(repo_root: &Path, dir_name: &str, sid: &str, watcher_pid: Option<u32>) {
    let sdir = repo_root.join("sessions").join(dir_name);
    std::fs::create_dir_all(&sdir).unwrap();
    let now = Utc::now().to_rfc3339();
    let meta = SessionMeta {
        version: 1,
        session_id: sid.into(),
        pid: Some(std::process::id()),
        started_at: now.clone(),
        last_touched: now,
        base_sha: "0".repeat(40),
        source_worktree: "/tmp".into(),
        overlay_version: 1,
        watcher_pid,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&sdir.join("session_meta.json"), &meta).unwrap();
}

#[test]
fn alive_peers_reads_session_meta_json_not_meta_json() {
    // Regression: pre-fix, alive_peers read `meta.json` while writers create
    // `session_meta.json`; resulting in `peers status` always reporting
    // "no peers" even with live sessions on disk.
    let dir = tempdir().unwrap();
    write_session(dir.path(), "peer-a", None);
    write_session(dir.path(), "peer-b", None);

    let peers_from_a = alive_peers(dir.path(), &peer_dir("peer-a"));
    assert_eq!(peers_from_a.len(), 1, "peer-a should see peer-b");
    assert_eq!(peers_from_a[0].session_id, peer_dir("peer-b"));

    let peers_from_b = alive_peers(dir.path(), &peer_dir("peer-b"));
    assert_eq!(peers_from_b.len(), 1, "peer-b should see peer-a");
    assert_eq!(peers_from_b[0].session_id, peer_dir("peer-a"));
}

#[test]
fn alive_peers_skips_stale_suffixed_dirs() {
    // Promotion case B retired a session as "<sid>.stale-<sha>" before it
    // used `.dead.*` names; leftovers of either shape must not appear as live
    // peers even if the original pid is still alive.
    let dir = tempdir().unwrap();
    write_session(dir.path(), "alive-session", None);
    for retired in [
        "alive-session~aaaaaaaa.stale-deadbeefcafebabe1234567890abcdef12345678",
        "alive-session~aaaaaaaa.dead.1700000000",
    ] {
        write_session_in(dir.path(), retired, "alive-session", None);
    }

    let peers = alive_peers(dir.path(), "other");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].session_id, peer_dir("alive-session"));
}

#[test]
fn watcher_pid_distinguishes_not_started_from_dead() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "no-watcher", None);
    write_session(dir.path(), "stale-watcher", Some(1)); // pid 1 (init) typically not signal-reachable
    write_session(dir.path(), "live-watcher", Some(std::process::id()));

    let peers = alive_peers(dir.path(), "other");
    let by_id = |name: &str| {
        let dir_name = peer_dir(name);
        peers.iter().find(|p| p.session_id == dir_name).unwrap()
    };

    let nw = by_id("no-watcher");
    assert!(nw.watcher_pid.is_none(), "no-watcher should expose None");
    assert!(!nw.watcher_alive);

    let sw = by_id("stale-watcher");
    assert_eq!(sw.watcher_pid, Some(1));
    assert!(
        !sw.watcher_alive,
        "pid 1 should be unreachable via signal probe"
    );

    let lw = by_id("live-watcher");
    assert_eq!(lw.watcher_pid, Some(std::process::id()));
    assert!(lw.watcher_alive);
}

#[test]
fn inbox_round_trip_targeted_message() {
    use ecp_core::peer::inbox::{append_entry, drain, InboxEntry};

    let dir = tempdir().unwrap();
    write_session(dir.path(), "sender", None);
    write_session(dir.path(), "receiver", None);

    let inbox = dir
        .path()
        .join("sessions")
        .join(peer_dir("receiver"))
        .join("inbox.jsonl");
    let msg = InboxEntry::Message {
        ts: Utc::now().to_rfc3339(),
        msg_id: "m_test1".into(),
        from: "sender".into(),
        from_name: None,
        to: Some("receiver".into()),
        reply_to: None,
        body: "ping".into(),
    };
    append_entry(&inbox, &msg).unwrap();

    let (entries, _wm) = drain(&inbox, 0).unwrap();
    assert_eq!(entries.len(), 1);
    match &entries[0] {
        InboxEntry::Message { body, from, to, .. } => {
            assert_eq!(body, "ping");
            assert_eq!(from, "sender");
            assert_eq!(to.as_deref(), Some("receiver"));
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn status_json_emits_well_formed_array_with_watcher_field() {
    use std::process::{Command, Stdio};

    let dir = tempdir().unwrap();
    let me = "e2e-self";
    let other = "e2e-other";
    write_own_session(dir.path(), me);
    write_session(dir.path(), other, Some(std::process::id()));

    let bin: std::path::PathBuf = env!("CARGO_BIN_EXE_ecp").into();
    let out = Command::new(&bin)
        .args([
            "peers",
            "--repo",
            dir.path().to_str().unwrap(),
            "status",
            "--format",
            "json",
        ])
        .env("ECP_SESSION_ID", me)
        .stdout(Stdio::piped())
        .output()
        .expect("ecp peers status --format json");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout: {stdout}"));
    let rows = parsed.as_array().expect("expected array");
    assert_eq!(rows.len(), 1, "should see exactly one peer (the 'other')");
    let row = &rows[0];
    assert_eq!(row["session_id"], peer_dir("e2e-other"));
    assert_eq!(row["watcher"], "alive");
    assert_eq!(row["watcher_pid"], std::process::id());
}

#[test]
fn status_text_reports_not_started_when_watcher_pid_absent() {
    use std::process::{Command, Stdio};

    let dir = tempdir().unwrap();
    write_own_session(dir.path(), "me-ns");
    write_session(dir.path(), "other-ns", None);

    let bin: std::path::PathBuf = env!("CARGO_BIN_EXE_ecp").into();
    let out = Command::new(&bin)
        .args(["peers", "--repo", dir.path().to_str().unwrap(), "status"])
        .env("ECP_SESSION_ID", "me-ns")
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("watcher=not-started"),
        "should show not-started; got: {stdout}"
    );
}

/// Contract: a peer listing holds only `<sid>~<h8>` dirs. A 0.17.3 bare dir,
/// a hex suffix of 7 or 9 chars, an upper-case suffix, and a hidden dir are
/// not peers; a sid that holds `~` is one peer, split at its last `~`.
#[test]
fn test_alive_peers_non_conforming_dir_names_are_never_listed() {
    let dir = tempdir().unwrap();
    write_session_in(dir.path(), "legacy", "legacy", None);
    for bad in [
        "short~1234abc",
        "long~1234abcde",
        "upper~1234ABCD",
        ".hidden~aaaaaaaa",
        "~aaaaaaaa",
    ] {
        write_session_in(dir.path(), bad, bad, None);
    }
    write_session_in(dir.path(), "has~tilde~1234abcd", "has~tilde", None);

    let ids: Vec<String> = alive_peers(dir.path(), "other")
        .into_iter()
        .map(|p| p.session_id)
        .collect();

    assert_eq!(ids, vec!["has~tilde~1234abcd".to_string()]);
}

#[test]
fn test_alive_peers_own_legacy_bare_dir_is_not_listed_as_peer() {
    let dir = tempdir().unwrap();
    write_session_in(dir.path(), "me", "me", None);
    write_session(dir.path(), "peer", None);

    let ids: Vec<String> = alive_peers(dir.path(), &peer_dir("me"))
        .into_iter()
        .map(|p| p.session_id)
        .collect();

    assert_eq!(ids, vec![peer_dir("peer")]);
}
