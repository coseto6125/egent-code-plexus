use cgn_cli::build::orchestrator::build_l2;
use cgn_cli::engine::{Engine, GraphView};
use cgn_core::session::{DirtyFiles, SessionMeta};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn git_init(p: &Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["init", "-q"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.email", "t@t"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.name", "t"])
        .status()
        .unwrap();
    fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["commit", "-qm", "init"])
        .status()
        .unwrap();
    let o = Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn put_session(repo_root: &Path, sid: &str, base_sha: &str, dirty: DirtyFiles) {
    let sd = repo_root.join("sessions").join(sid);
    fs::create_dir_all(&sd).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: sid.into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: base_sha.into(),
        source_worktree: "/tmp/wt".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sd.join("session_meta.json"), &sm).unwrap();
    DirtyFiles::write_atomic(&sd.join("dirty_files.json"), &dirty).unwrap();
}

#[test]
fn engine_open_pure_reference_loads_l2only() {
    let _g = HOME_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let sha = git_init(wt.path());
    std::env::set_var("HOME", home.path());

    // Build a real L2 so Engine::load can mmap a valid graph.bin
    let initial = build_l2(wt.path(), None).unwrap();
    let repo_root = initial
        .commit_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    put_session(&repo_root, "sid_pure", &sha, DirtyFiles::empty());

    let engine = Engine::open(&repo_root, "sid_pure").unwrap();
    assert_eq!(engine.view(), GraphView::L2Only);
}

#[test]
fn engine_open_stale_session_returns_err() {
    let _g = HOME_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());
    std::env::set_var("HOME", home.path());

    let initial = build_l2(wt.path(), None).unwrap();
    let repo_root = initial
        .commit_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    // Create a session dir without session_meta.json → MetaUnreadable → Stale
    fs::create_dir_all(repo_root.join("sessions/sid_broken")).unwrap();

    let r = Engine::open(&repo_root, "sid_broken");
    assert!(r.is_err(), "Stale session should fail to open");
}
