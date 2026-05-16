use graph_nexus_cli::session::promotion::{
    promote_case_a, promote_case_b, promotion_case, PromotionCase,
};
use graph_nexus_core::session::{DirtyEntry, DirtyFiles, SessionMeta};
use std::process::Command;

fn git_init_with_commit(p: &std::path::Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .arg("init")
        .arg("-q")
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
    std::fs::write(p.join("a.rs"), "fn a() {}").unwrap();
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
    head(p)
}

fn head(p: &std::path::Path) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn make_session(
    tmp: &std::path::Path,
    base_sha: &str,
    source_wt: &std::path::Path,
) -> std::path::PathBuf {
    let sessions = tmp.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let sid = sessions.join("test-sid");
    std::fs::create_dir(&sid).unwrap();
    std::fs::create_dir(sid.join("graph_overlay")).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "test-sid".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: "2026-05-17T10:00:00Z".into(),
        base_sha: base_sha.to_string(),
        source_worktree: source_wt.to_string_lossy().into(),
        overlay_version: 0,
    };
    SessionMeta::write_atomic(&sid.join("session_meta.json"), &sm).unwrap();
    DirtyFiles::write_atomic(&sid.join("dirty_files.json"), &DirtyFiles::empty()).unwrap();
    sid
}

#[test]
fn fast_forward_is_case_a() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    let old_sha = git_init_with_commit(wt);
    std::fs::write(wt.join("a.rs"), "fn a() { 1 }").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["commit", "-qm", "x"])
        .status()
        .unwrap();
    let new_sha = head(wt);
    assert_eq!(promotion_case(&old_sha, &new_sha, wt), PromotionCase::A);
}

#[test]
fn going_backward_is_case_b() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    let main_sha = git_init_with_commit(wt);
    Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["checkout", "-q", "-b", "side"])
        .status()
        .unwrap();
    std::fs::write(wt.join("b.rs"), "fn b() {}").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt)
        .args(["commit", "-qm", "side"])
        .status()
        .unwrap();
    let side_sha = head(wt);
    // Going from side back to main: merge-base(side, main) = main_sha != side_sha (old) → B
    assert_eq!(promotion_case(&side_sha, &main_sha, wt), PromotionCase::B);
}

#[test]
fn case_a_drops_fragment_when_content_matches_new_l2() {
    let tmp_session_root = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let wt_path = wt.path();
    let old_sha = git_init_with_commit(wt_path);

    // Edit a.rs and commit — new_sha will have the new content.
    let new_content = "fn a() { 42 }";
    std::fs::write(wt_path.join("a.rs"), new_content).unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt_path)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt_path)
        .args(["commit", "-qm", "edit"])
        .status()
        .unwrap();
    let new_sha = head(wt_path);

    // Build a session with a dirty entry whose content_hash matches the new_sha's blob.
    let sid_dir = make_session(tmp_session_root.path(), &old_sha, wt_path);
    let h = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(new_content.as_bytes()))
    };
    let frag_id = h[..16].to_string();
    let frag_path = sid_dir.join("graph_overlay").join(format!("{frag_id}.bin"));
    std::fs::write(&frag_path, b"stub fragment").unwrap();
    let mut df = DirtyFiles::empty();
    df.entries.insert(
        "a.rs".into(),
        DirtyEntry {
            mtime_ns: 0,
            content_hash: h,
            fragment_id: frag_id,
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: vec![],
        },
    );
    DirtyFiles::write_atomic(&sid_dir.join("dirty_files.json"), &df).unwrap();

    let stats = promote_case_a(&sid_dir, wt_path, &new_sha).unwrap();
    assert_eq!(stats.dropped, 1);
    assert_eq!(stats.kept, 0);
    assert!(!frag_path.exists(), "fragment file should be removed");
    let df2 = DirtyFiles::read(&sid_dir.join("dirty_files.json")).unwrap();
    assert!(df2.entries.is_empty());

    let sm = SessionMeta::read(&sid_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm.base_sha, new_sha);
}

#[test]
fn case_a_keeps_fragment_when_content_diverges() {
    let tmp_session_root = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let wt_path = wt.path();
    let old_sha = git_init_with_commit(wt_path);

    // Commit a different value than what's in L1.
    std::fs::write(wt_path.join("a.rs"), "fn a() { 99 }").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt_path)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(wt_path)
        .args(["commit", "-qm", "edit"])
        .status()
        .unwrap();
    let new_sha = head(wt_path);

    // L1 has a different content (not what was committed).
    let sid_dir = make_session(tmp_session_root.path(), &old_sha, wt_path);
    let frag_id = "abcdefgh12345678";
    let frag_path = sid_dir.join("graph_overlay").join(format!("{frag_id}.bin"));
    std::fs::write(&frag_path, b"stub fragment").unwrap();
    let mut df = DirtyFiles::empty();
    df.entries.insert(
        "a.rs".into(),
        DirtyEntry {
            mtime_ns: 0,
            content_hash: "0".repeat(64), // intentionally mismatched hash
            fragment_id: frag_id.into(),
            tantivy_delta_segment: None,
            parse_failed: false,
            dirty_symbols: vec![],
        },
    );
    DirtyFiles::write_atomic(&sid_dir.join("dirty_files.json"), &df).unwrap();

    let stats = promote_case_a(&sid_dir, wt_path, &new_sha).unwrap();
    assert_eq!(stats.dropped, 0);
    assert_eq!(stats.kept, 1);
    assert!(frag_path.exists(), "fragment file should remain");
}

#[test]
fn case_b_atomic_renames_to_stale_then_recreates() {
    let tmp = tempfile::tempdir().unwrap();
    let old_sha = "1".repeat(40);
    let new_sha = "2".repeat(40);
    let wt = tempfile::tempdir().unwrap();
    let sid_dir = make_session(tmp.path(), &old_sha, wt.path());

    // Write some fragment files.
    std::fs::write(sid_dir.join("graph_overlay").join("a.bin"), b"x").unwrap();

    promote_case_b(&sid_dir, &old_sha, &new_sha).unwrap();

    // session_dir must exist (fresh) and be empty of fragments.
    assert!(sid_dir.exists());
    let sm = SessionMeta::read(&sid_dir.join("session_meta.json")).unwrap();
    assert_eq!(sm.base_sha, new_sha);
    assert_eq!(sm.overlay_version, 0);

    let df = DirtyFiles::read(&sid_dir.join("dirty_files.json")).unwrap();
    assert!(df.entries.is_empty());

    // Stale dir should exist (background GC hasn't run yet).
    let stale = sid_dir
        .parent()
        .unwrap()
        .join(format!("test-sid.stale-{old_sha}"));
    assert!(
        stale.exists(),
        "stale dir must exist immediately after rename"
    );

    // Sleep briefly to allow background GC to fire.
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(
        !stale.exists(),
        "background GC should have removed stale dir after 2s"
    );
}
