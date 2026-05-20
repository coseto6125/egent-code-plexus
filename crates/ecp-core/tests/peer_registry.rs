use chrono::Utc;
use ecp_core::peer::registry::alive_peers;
use ecp_core::registry::atomic_write_json;
use ecp_core::session::SessionMeta;
use std::fs;
use tempfile::tempdir;

fn write_meta(root: &std::path::Path, id: &str, pid: u32) {
    let dir = root.join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let meta = SessionMeta {
        version: 1,
        session_id: id.into(),
        pid: Some(pid),
        started_at: Utc::now().to_rfc3339(),
        last_touched: Utc::now().to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: "/tmp".into(),
        overlay_version: 1,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    atomic_write_json(&dir.join("meta.json"), &meta).unwrap();
}

#[test]
fn alive_peers_excludes_self_and_dead_pids() {
    let dir = tempdir().unwrap();
    write_meta(dir.path(), "self", std::process::id());
    write_meta(dir.path(), "alive_peer", std::process::id());
    write_meta(dir.path(), "dead_peer", 999_999_999);

    let peers = alive_peers(dir.path(), "self");
    let ids: Vec<_> = peers.iter().map(|p| p.session_id.as_str()).collect();
    assert!(ids.contains(&"alive_peer"));
    assert!(!ids.contains(&"self"));
    assert!(!ids.contains(&"dead_peer"));
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
    };
    atomic_write_json(&s.join("meta.json"), &meta).unwrap();
    let peers = alive_peers(dir.path(), "self");
    assert!(
        peers.iter().all(|p| p.session_id != "broken"),
        "session with unparseable timestamp must be filtered out"
    );
}
