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

fn write_dirty(root: &std::path::Path, id: &str, symbols: &[&str]) {
    write_dirty_in(root, id, "src/a.ts", symbols);
}

fn write_dirty_in(root: &std::path::Path, id: &str, file: &str, symbols: &[&str]) {
    use ecp_core::contracts::dirty_event::{PathChange, PathClass};
    use ecp_core::contracts::routing::SizeCap;
    use ecp_core::session::overlay::{DirtyEventEntry, DirtyEvents, SymbolKind, SymbolRef};
    let mut manifest = DirtyEvents::empty(SizeCap::BUILT_IN);
    manifest.entries.insert(
        file.to_string(),
        DirtyEventEntry {
            change: PathChange::Modified,
            class: PathClass::SOURCE,
            mtime_ns: 0,
            fragment_id: Some("f".into()),
            content_hash: Some("h".into()),
            parse_failed: false,
            dirty_symbols: symbols
                .iter()
                .map(|s| SymbolRef {
                    name: (*s).into(),
                    kind: SymbolKind::Function,
                    file: file.into(),
                    line_start: 1,
                    line_end: 2,
                })
                .collect(),
            format: ecp_core::session::overlay::FRAGMENT_FORMAT_V5,
        },
    );
    DirtyEvents::write_atomic(
        &ecp_core::session::layout::dirty_events_path(&root.join("sessions").join(id)),
        &manifest,
    )
    .unwrap();
}

#[test]
fn peers_status_pairs_reports_hard_overlap_matrix() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-one", Some("worker-a"));
    write_session(dir.path(), "s-two", Some("worker-b"));
    write_session(dir.path(), "s-three", None);
    // HARD is a shared dirty FILE: s-one and s-two collide on src/a.ts,
    // s-three is alone in src/z.ts.
    write_dirty(dir.path(), "s-one", &["verify_token", "parse_call"]);
    write_dirty(dir.path(), "s-two", &["verify_token"]);
    write_dirty_in(dir.path(), "s-three", "src/z.ts", &["unrelated_fn"]);

    let out = Command::new(bin())
        .args([
            "peers",
            "status",
            "--pairs",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-pairs")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("worker-a") && stdout.contains("worker-b"),
        "overlapping pair must be named: {stdout}"
    );
    assert!(
        stdout.contains("src/a.ts"),
        "the shared dirty file must be listed: {stdout}"
    );
    assert!(
        !stdout.contains("src/z.ts"),
        "disjoint session must not appear as overlap: {stdout}"
    );
}

#[test]
fn peers_status_pairs_disjoint_says_none() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-one", None);
    write_session(dir.path(), "s-two", None);
    write_dirty_in(dir.path(), "s-one", "src/a.ts", &["fn_a"]);
    write_dirty_in(dir.path(), "s-two", "src/b.ts", &["fn_b"]);

    let out = Command::new(bin())
        .args([
            "peers",
            "status",
            "--pairs",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-pairs2")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no overlapping pairs"),
        "disjoint matrix must say so plainly: {stdout}"
    );
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

/// Contract: a peer's deleted file prints as `(deleted)` with no symbol list;
/// an edited file beside it keeps its symbols.
#[test]
fn test_peers_diff_tombstone_prints_deleted_marker() {
    use ecp_core::contracts::dirty_event::PathClass;
    use ecp_core::session::overlay::{DirtyEventEntry, DirtyEvents};
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-peer", None);
    write_dirty_in(dir.path(), "s-peer", "src/edited.ts", &["edited_fn"]);
    let manifest_path =
        ecp_core::session::layout::dirty_events_path(&dir.path().join("sessions").join("s-peer"));
    let mut manifest = DirtyEvents::read(&manifest_path).unwrap();
    manifest.entries.insert(
        "src/gone.ts".into(),
        DirtyEventEntry::tombstone(PathClass::SOURCE),
    );
    DirtyEvents::write_atomic(&manifest_path, &manifest).unwrap();

    let out = Command::new(bin())
        .args([
            "peers",
            "diff",
            "s-peer",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-diff")
        .output()
        .expect("spawn");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines.contains(&"--- src/gone.ts (deleted) ---"), "{stdout}");
    assert!(lines.contains(&"--- src/edited.ts ---"), "{stdout}");
    assert!(stdout.contains("edited_fn"), "{stdout}");
}
