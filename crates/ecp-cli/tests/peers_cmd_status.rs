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

/// A peer's session dir name: `<sid>~<h8>`, the only shape a peer listing
/// accepts. An `id` that already carries its worktree key is kept as is.
fn peer_dir(id: &str) -> String {
    if id.contains('~') {
        id.to_string()
    } else {
        format!("{id}~aaaaaaaa")
    }
}

fn write_session(root: &std::path::Path, id: &str, agent_name: Option<&str>) {
    use ecp_core::session::SessionMeta;
    let dir = root.join("sessions").join(peer_dir(id));
    std::fs::create_dir_all(&dir).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let meta = SessionMeta {
        version: 1,
        session_id: peer_dir(id),
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
    write_dirty_hashed(root, id, file, symbols, "h");
}

fn write_dirty_hashed(root: &std::path::Path, id: &str, file: &str, symbols: &[&str], hash: &str) {
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
            content_hash: Some(hash.into()),
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
            format: ecp_core::session::overlay::FRAGMENT_FORMAT_V8,
        },
    );
    DirtyEvents::write_atomic(
        &ecp_core::session::layout::dirty_events_path(&root.join("sessions").join(peer_dir(id))),
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

/// Fails before the fix: two worktrees that pulled one commit hold the same
/// text for its files, and every such file was listed as an overlap.
#[test]
fn peers_status_pairs_other_worktrees_same_text_says_none() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-one~aaaaaaaa", None);
    write_session(dir.path(), "s-two~bbbbbbbb", None);
    write_dirty_in(dir.path(), "s-one~aaaaaaaa", "src/a.ts", &["fn_a"]);
    write_dirty_in(dir.path(), "s-two~bbbbbbbb", "src/a.ts", &["fn_a"]);

    let out = Command::new(bin())
        .args([
            "peers",
            "status",
            "--pairs",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-pairs3")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no overlapping pairs"),
        "equal text in two worktrees is no overlap: {stdout}"
    );
}

#[test]
fn peers_status_pairs_other_worktrees_different_text_lists_the_file() {
    let dir = tempdir().unwrap();
    write_session(dir.path(), "s-one~aaaaaaaa", None);
    write_session(dir.path(), "s-two~bbbbbbbb", None);
    write_dirty_hashed(dir.path(), "s-one~aaaaaaaa", "src/a.ts", &["fn_a"], "h1");
    write_dirty_hashed(dir.path(), "s-two~bbbbbbbb", "src/a.ts", &["fn_a"], "h2");

    let out = Command::new(bin())
        .args([
            "peers",
            "status",
            "--pairs",
            "--repo",
            dir.path().to_str().unwrap(),
        ])
        .env("ECP_SESSION_ID", "me-pairs4")
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("src/a.ts") && !stdout.contains("no overlapping pairs"),
        "different text in two worktrees is an overlap: {stdout}"
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
        stdout.contains("session=s-named~aaaaaaaa\tname=rust-parser"),
        "named peer missing name column: {stdout}"
    );
    assert!(
        stdout.contains("session=s-anon~aaaaaaaa\tname=-"),
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
    assert_eq!(by_id(&peer_dir("s-named"))["agent_name"], "rust-parser");
    assert_eq!(
        by_id(&peer_dir("s-anon"))["agent_name"],
        serde_json::Value::Null
    );
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
    let manifest_path = ecp_core::session::layout::dirty_events_path(
        &dir.path().join("sessions").join(peer_dir("s-peer")),
    );
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

/// Contract: one session id in two worktrees is two peers, one per worktree,
/// each with the dirty files of its own tree. Before the session dir carried
/// the worktree, the second worktree's query replaced the first one's
/// manifest and `peers status` listed one row.
#[test]
fn test_peers_status_two_worktrees_one_session_lists_each_worktree() {
    use ecp_cli::repo_identity::repo_dir_name_for_cwd;
    use ecp_cli::session::resolver::session_dir_name;

    let tmp = tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    let wt_b = tmp.path().join("wt-b");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("app.py"), "def keep_me():\n    return 1\n").unwrap();
    std::fs::write(repo.join("lib.py"), "def helper_b():\n    return 3\n").unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        assert!(Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    };
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
            wt_b.to_str().unwrap(),
        ],
    );
    let ecp = |session: &str| {
        let mut cmd = Command::new(bin());
        cmd.env("HOME", &home)
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", session);
        cmd
    };
    assert!(ecp("shared")
        .args(["admin", "index", "--repo"])
        .arg(&repo)
        .status()
        .unwrap()
        .success());
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(repo.join("app.py"), "# pad\ndef keep_me():\n    return 1\n").unwrap();
    std::fs::write(
        wt_b.join("lib.py"),
        "# pad\ndef helper_b():\n    return 3\n",
    )
    .unwrap();
    for (worktree, name) in [(&repo, "keep_me"), (&wt_b, "helper_b")] {
        let out = ecp("shared")
            .args(["find", name, "--repo"])
            .arg(worktree)
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    let repo_root = home
        .join(".ecp")
        .join(repo_dir_name_for_cwd(&repo).unwrap());
    let id_a = session_dir_name("shared", &repo);
    let id_b = session_dir_name("shared", &wt_b);
    let status = ecp("observer")
        .args(["peers", "status", "--format", "json", "--repo"])
        .arg(&repo_root)
        .output()
        .unwrap();
    assert!(status.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&status.stdout).unwrap();
    let mut ids: Vec<&str> = rows
        .iter()
        .map(|r| r["session_id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut expected = vec![id_a.as_str(), id_b.as_str()];
    expected.sort();
    assert_eq!(ids, expected, "one row per (session, worktree)");

    for (id, own, other) in [(&id_a, "app.py", "lib.py"), (&id_b, "lib.py", "app.py")] {
        let diff = ecp("observer")
            .args(["peers", "diff", id, "--repo"])
            .arg(&repo_root)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&diff.stdout);
        assert!(
            diff.status.success(),
            "{}",
            String::from_utf8_lossy(&diff.stderr)
        );
        assert!(
            text.contains(&format!("--- {own} ---")) && !text.contains(other),
            "{id} holds only {own}: {text}"
        );
    }
}
