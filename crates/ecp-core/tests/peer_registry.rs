use chrono::Utc;
use ecp_core::peer::registry::{alive_peers, SESSION_LIVE_TTL_MINS};
use ecp_core::registry::atomic_write_json;
use ecp_core::session::SessionMeta;
use std::fs;
use tempfile::tempdir;

fn write_meta(root: &std::path::Path, id: &str, pid: u32) {
    write_meta_full(root, id, Some(pid), None, Utc::now().to_rfc3339());
}

fn write_meta_full(
    root: &std::path::Path,
    id: &str,
    pid: Option<u32>,
    watcher_pid: Option<u32>,
    last_touched: String,
) {
    let dir = root.join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: id.into(),
        pid,
        started_at: Utc::now().to_rfc3339(),
        last_touched,
        base_sha: "0".repeat(40),
        source_worktree: "/tmp".into(),
        overlay_version: 1,
        watcher_pid,
        last_drained_offset: 0,
        agent_name: None,
    };
    atomic_write_json(&dir.join("session_meta.json"), &meta).unwrap();
}

fn ago(mins: i64) -> String {
    (Utc::now() - chrono::Duration::minutes(mins)).to_rfc3339()
}

#[test]
fn alive_peers_excludes_self_and_idle_sessions() {
    let dir = tempdir().unwrap();
    write_meta(dir.path(), "self", std::process::id());
    write_meta(dir.path(), "alive_peer", std::process::id());
    write_meta_full(
        dir.path(),
        "idle_peer",
        Some(999_999_999),
        None,
        ago(SESSION_LIVE_TTL_MINS + 5),
    );

    let peers = alive_peers(dir.path(), "self");
    let ids: Vec<_> = peers.iter().map(|p| p.session_id.as_str()).collect();
    assert!(ids.contains(&"alive_peer"));
    assert!(!ids.contains(&"self"));
    assert!(
        !ids.contains(&"idle_peer"),
        "a dead pid with no activity inside the TTL is a corpse"
    );
}

/// The recorded pid belongs to the one-shot `ecp` process that wrote the meta.
/// It is dead before any other session can read it, so a dead pid alone must
/// not hide a session that was active moments ago (FU-2026-06-10-cc120f78889c).
#[test]
fn alive_peers_keeps_dead_pid_when_recently_touched() {
    let dir = tempdir().unwrap();
    write_meta_full(dir.path(), "stateless", Some(999_999_999), None, ago(1));

    let ids: Vec<_> = alive_peers(dir.path(), "self")
        .into_iter()
        .map(|p| p.session_id)
        .collect();
    assert_eq!(ids, vec!["stateless".to_string()]);
}

/// The watch path enrols with `pid: null`. Dropping those made every
/// watcher-backed session invisible and left the dirs to accumulate.
#[test]
fn alive_peers_keeps_null_pid_when_watcher_alive() {
    let dir = tempdir().unwrap();
    write_meta_full(
        dir.path(),
        "watched",
        None,
        Some(std::process::id()),
        ago(SESSION_LIVE_TTL_MINS + 5),
    );

    let peers = alive_peers(dir.path(), "self");
    assert_eq!(peers.len(), 1, "a live watcher keeps its session visible");
    assert!(peers[0].watcher_alive);
    assert_eq!(peers[0].pid, None);
}

#[test]
fn alive_peers_drops_null_pid_when_watcher_dead_and_idle() {
    let dir = tempdir().unwrap();
    write_meta_full(
        dir.path(),
        "zombie",
        None,
        Some(999_999_999),
        ago(SESSION_LIVE_TTL_MINS + 5),
    );

    assert!(
        alive_peers(dir.path(), "self").is_empty(),
        "no live watcher and no recent activity means the session is gone"
    );
}

#[test]
fn alive_peers_empty_when_no_sessions() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("sessions")).unwrap();
    assert!(alive_peers(dir.path(), "self").is_empty());
}

#[test]
fn pid_alive_returns_false_for_pid_zero_and_one() {
    use ecp_core::peer::registry::pid_alive;
    assert!(
        !pid_alive(0),
        "pid=0 must be treated as not alive (would target process group)"
    );
    assert!(
        !pid_alive(1),
        "pid=1 (init) must not be treated as a real peer session"
    );
}

#[test]
fn alive_peers_skips_session_with_unparseable_timestamp() {
    let dir = tempdir().unwrap();
    let s = dir.path().join("sessions/broken");
    fs::create_dir_all(&s).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: "broken".into(),
        pid: Some(std::process::id()),
        started_at: Utc::now().to_rfc3339(),
        last_touched: "this-is-not-a-timestamp".into(),
        base_sha: "0".repeat(40),
        source_worktree: "/tmp".into(),
        overlay_version: 1,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    atomic_write_json(&s.join("session_meta.json"), &meta).unwrap();
    let peers = alive_peers(dir.path(), "self");
    assert!(
        peers.iter().all(|p| p.session_id != "broken"),
        "session with unparseable timestamp must be filtered out"
    );
}
