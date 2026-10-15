//! `admin gc` and the overlay writer meet at a session's fragment bins. A
//! bin's name is its content hash, so a writer can re-publish the very bin gc
//! judged orphan; the overlay lock orders the two, and the freshness gate
//! re-parses a file whose bin is gone whatever removed it.

use ecp_core::session::{DirtyFiles, SessionMeta};
use std::path::{Path, PathBuf};
use std::process::Command;

fn session_meta(session: &Path) {
    let sm = SessionMeta {
        version: 1,
        session_id: "sid".into(),
        pid: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: String::new(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&session.join("session_meta.json"), &sm).unwrap();
}

/// While gc holds the session, the writer publishes no bin and commits no
/// manifest; once gc lets go, it does both.
#[test]
fn test_write_dirty_fragments_from_graphs_overlay_lock_held_publishes_after_release() {
    let tmp = tempfile::tempdir().unwrap();
    let session = tmp.path().join("sid");
    std::fs::create_dir_all(&session).unwrap();
    session_meta(&session);
    let held = ecp_core::registry::FileLock::try_exclusive(
        &ecp_cli::session::overlay_writer::overlay_lock_path(&session),
    )
    .unwrap();
    let hash = [7u8; 8];
    let bin = session
        .join("graph_overlay")
        .join(format!("{:016x}.bin", u64::from_le_bytes(hash)));
    let writer = {
        let session = session.clone();
        std::thread::spawn(move || {
            let graph = ecp_core::analyzer::types::LocalGraph {
                file_path: "a.py".into(),
                content_hash: hash,
                ..Default::default()
            };
            ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs(
                &session,
                &[graph],
                &[1],
            )
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !bin.exists(),
        "a bin was published while gc held the session"
    );
    assert!(
        !session.join("dirty_files.json").exists(),
        "a manifest was committed while gc held the session"
    );
    drop(held);
    writer.join().unwrap().unwrap();
    assert!(bin.exists(), "the writer never published after the release");
}

fn run(cmd: &mut Command, label: &str) -> std::process::Output {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{label} spawn failed: {e}"));
    assert!(
        out.status.success(),
        "{label} failed:\n  stderr={}\n  stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    out
}

fn git(repo: &Path, args: &[&str]) {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE"),
        "git",
    );
}

fn ecp(home: &Path, args: &[&str]) -> std::process::Output {
    run(
        Command::new(env!("CARGO_BIN_EXE_ecp"))
            .args(args)
            .env("HOME", home)
            .env("ECP_HOME", home)
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env("CLAUDE_CODE_SESSION_ID", "test-fragment-self-heal"),
        "ecp",
    )
}

fn find_json(repo: &Path, home: &Path, name: &str) -> serde_json::Value {
    let out = ecp(
        home,
        &[
            "find",
            name,
            "--format",
            "json",
            "--repo",
            repo.to_str().unwrap(),
        ],
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid find JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn overlay_dirs(home: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(home)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_dir() && e.file_name() == "graph_overlay")
        .map(|e| e.into_path())
        .collect()
}

/// The manifest still names the dirty file's bin at the file's mtime, but
/// the bin is gone. Before the gate checked the bin, the query trusted the
/// manifest, skipped the re-parse, and the reader, which skips a missing
/// bin, served the committed graph until the file changed again.
#[test]
fn test_find_fragment_bin_missing_under_current_manifest_reparsed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/a.rs"), "fn original() {}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);
    ecp(&home, &["admin", "index", "--repo", repo.to_str().unwrap()]);

    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(
        repo.join("src/a.rs"),
        "fn original() {}\nfn healed_probe() {}\n",
    )
    .unwrap();
    let first = find_json(&repo, &home, "healed_probe");
    assert_eq!(first["found"], true, "overlay never served: {first}");

    let overlays = overlay_dirs(&home);
    assert_eq!(overlays.len(), 1, "{overlays:?}");
    let overlay = &overlays[0];
    let manifest = DirtyFiles::read(&overlay.parent().unwrap().join("dirty_files.json")).unwrap();
    let entry = manifest
        .entries
        .get("src/a.rs")
        .expect("the dirty file has no manifest entry");
    let bin = overlay.join(format!("{}.bin", entry.fragment_id));
    std::fs::remove_file(&bin).unwrap();

    let second = find_json(&repo, &home, "healed_probe");
    assert!(bin.exists(), "the missing bin was never re-published");
    assert_eq!(
        second["found"], true,
        "the query served the committed graph: {second}"
    );
}
