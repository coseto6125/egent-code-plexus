use ecp_cli::admin::gc::{enforce_quota, reachability, sweep_sessions, sweep_stale_generations};
use ecp_core::registry::{CommitBuildMeta, EmbeddingStatus, SourceType};
use ecp_core::session::SessionMeta;
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
    std::fs::write(p.join("a"), "x").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["commit", "-qm", "x"])
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

#[test]
fn reachability_includes_branch_refs_and_active_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    let main_sha = git_init_with_commit(&wt);

    let repo_root = tmp.path().join("repo_root");
    let sessions = repo_root.join("sessions").join("sid1");
    std::fs::create_dir_all(&sessions).unwrap();
    let session_sha = "0".repeat(40);
    let sm = SessionMeta {
        version: 1,
        session_id: "sid1".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: session_sha.clone(),
        source_worktree: wt.to_string_lossy().into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sessions.join("session_meta.json"), &sm).unwrap();

    let r = reachability(&repo_root, &wt).unwrap();
    assert!(r.contains(&main_sha), "missing branch ref sha: {r:?}");
    assert!(r.contains(&session_sha), "missing session base sha: {r:?}");
}

#[test]
fn reachability_excludes_idle_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    let _main_sha = git_init_with_commit(&wt);

    let repo_root = tmp.path().join("repo_root");
    let sessions = repo_root.join("sessions").join("old-sid");
    std::fs::create_dir_all(&sessions).unwrap();
    let session_sha = "1".repeat(40);
    let old = chrono::Utc::now() - chrono::Duration::hours(48);
    let sm = SessionMeta {
        version: 1,
        session_id: "old-sid".into(),
        pid: None,
        started_at: old.to_rfc3339(),
        last_touched: old.to_rfc3339(),
        base_sha: session_sha.clone(),
        source_worktree: wt.to_string_lossy().into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sessions.join("session_meta.json"), &sm).unwrap();

    let r = reachability(&repo_root, &wt).unwrap();
    assert!(
        !r.contains(&session_sha),
        "idle session sha must not be reachable"
    );
}

fn make_commit_dir(commits: &std::path::Path, sha_hex: &str, built_at: &str, size_bytes: usize) {
    let dirname = format!("commit__{sha_hex}");
    let dir = commits.join(&dirname);
    std::fs::create_dir_all(&dir).unwrap();
    let meta = CommitBuildMeta {
        version: 1,
        sha: sha_hex.into(),
        source_type: SourceType::Commit,
        source_id: None,
        built_from_worktree: "/work".into(),
        built_at: built_at.into(),
        parent_sha: None,
        node_count: 0,
        embedding_status: EmbeddingStatus::None,
        refs_at_build: vec![],
        refs_seen_since: vec![],
        builder_fingerprint: None,
        binary_commit_sha: None,
    };
    CommitBuildMeta::write_atomic(&dir.join("meta.json"), &meta).unwrap();
    std::fs::write(dir.join("graph.bin"), vec![0u8; size_bytes]).unwrap();
}

#[test]
fn enforce_quota_evicts_oldest_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    let _main_sha = git_init_with_commit(&wt);

    let repo_root = tmp.path().join("repo_root");
    let commits = repo_root.join("commits");
    std::fs::create_dir_all(&commits).unwrap();

    // Three commit dirs; none reachable (random SHAs)
    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);
    let sha_c = "c".repeat(40);
    make_commit_dir(&commits, &sha_a, "2026-01-01T00:00:00Z", 10_000); // oldest
    make_commit_dir(&commits, &sha_b, "2026-03-01T00:00:00Z", 10_000);
    make_commit_dir(&commits, &sha_c, "2026-05-01T00:00:00Z", 10_000); // newest

    // Tiny quota: forces eviction down to 80% × quota
    let stats = enforce_quota(&repo_root, &wt, 15_000).unwrap();
    assert!(
        stats.evicted >= 1,
        "expected eviction, got: {} evicted",
        stats.evicted
    );

    // Oldest (sha_a) should be the one gone first
    assert!(
        !commits.join(format!("commit__{sha_a}")).exists(),
        "oldest should be evicted"
    );
}

#[test]
fn enforce_quota_protects_reachable_even_when_oldest() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    let main_sha = git_init_with_commit(&wt);

    let repo_root = tmp.path().join("repo_root");
    let commits = repo_root.join("commits");
    std::fs::create_dir_all(&commits).unwrap();

    // Two dirs: main_sha (reachable, OLD built_at) + sha_b (unreachable, newer)
    make_commit_dir(&commits, &main_sha, "2026-01-01T00:00:00Z", 10_000);
    let sha_b = "b".repeat(40);
    make_commit_dir(&commits, &sha_b, "2026-05-01T00:00:00Z", 10_000);

    enforce_quota(&repo_root, &wt, 15_000).unwrap();
    // main_sha must survive (reachable), sha_b evicted (unreachable)
    assert!(
        commits.join(format!("commit__{main_sha}")).exists(),
        "reachable main_sha must survive eviction"
    );
    assert!(
        !commits.join(format!("commit__{sha_b}")).exists(),
        "unreachable sha_b should be evicted"
    );
}

#[test]
fn sweep_sessions_marks_idle_sessions_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();
    let sessions = repo_root.join("sessions").join("old-sid");
    std::fs::create_dir_all(&sessions).unwrap();
    let old = chrono::Utc::now() - chrono::Duration::hours(48);
    let sm = SessionMeta {
        version: 1,
        session_id: "old-sid".into(),
        pid: None, // skip pid check on Unix
        started_at: old.to_rfc3339(),
        last_touched: old.to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: "/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
    };
    SessionMeta::write_atomic(&sessions.join("session_meta.json"), &sm).unwrap();

    let stats = sweep_sessions(repo_root).unwrap();
    assert_eq!(stats.marked, 1);
    assert!(!sessions.exists());
    let sessions_dir = repo_root.join("sessions");
    let dead_dir_exists = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("old-sid.dead")
        });
    assert!(
        dead_dir_exists,
        ".dead.<ts> rename expected; entries: {:?}",
        std::fs::read_dir(&sessions_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
}

#[test]
fn sweep_sessions_removes_already_marked_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();
    let dead_dir = repo_root.join("sessions").join("zombie.dead");
    std::fs::create_dir_all(&dead_dir).unwrap();
    std::fs::write(dead_dir.join("x"), b"junk").unwrap();

    let stats = sweep_sessions(repo_root).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!dead_dir.exists());
}

/// Create a commit dir with an explicit on-disk name (supports `.gen.<...>` suffixes).
/// Backdates mtime so the impl's fresh-guard (<10s) does not skip it.
fn make_named_commit_dir(commits: &std::path::Path, dir_name: &str) {
    let dir = commits.join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("graph.bin"), vec![0u8; 16]).unwrap();
    // Backdate so the impl's fresh-guard (<10s) doesn't skip it.
    filetime::set_file_mtime(&dir, filetime::FileTime::from_unix_time(1_000_000_000, 0)).unwrap();
}

#[test]
fn sweep_stale_generations_keeps_newest_per_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.1000.10.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.2000.20.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha_a}.gen.3000.30.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha_b}.gen.1500.15.0"));

    let stats = sweep_stale_generations(tmp.path()).unwrap();

    assert_eq!(stats.removed, 2, "two older same-SHA generations removed");
    assert!(commits
        .join(format!("branch_main__{sha_a}.gen.3000.30.0"))
        .exists());
    assert!(!commits
        .join(format!("branch_main__{sha_a}.gen.1000.10.0"))
        .exists());
    assert!(!commits
        .join(format!("branch_main__{sha_a}.gen.2000.20.0"))
        .exists());
    assert!(commits
        .join(format!("branch_main__{sha_b}.gen.1500.15.0"))
        .exists());
}

#[test]
fn sweep_stale_generations_skips_building() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "c".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    let older = format!("branch_main__{sha}.gen.500.5.0");
    make_named_commit_dir(&commits, &older);
    // Build the `.building` sibling at EXACTLY the path the impl checks.
    let building = commits.join(&older).with_extension("building");
    std::fs::create_dir_all(&building).unwrap();

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();
    assert_eq!(
        stats.removed, 0,
        "older dir guarded by .building sibling must stay"
    );
    assert!(commits.join(&older).exists());
}

#[test]
fn sweep_stale_generations_skips_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "d".repeat(40);
    // Create two same-SHA gens, then RESET their mtime to now (fresh <10s) so the guard skips.
    let g1 = format!("branch_main__{sha}.gen.1000.10.0");
    let g2 = format!("branch_main__{sha}.gen.2000.20.0");
    make_named_commit_dir(&commits, &g1);
    make_named_commit_dir(&commits, &g2);
    // make_named_commit_dir backdates mtime; override back to NOW so they're fresh.
    let now = filetime::FileTime::now();
    filetime::set_file_mtime(commits.join(&g1), now).unwrap();
    filetime::set_file_mtime(commits.join(&g2), now).unwrap();

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0, "freshly-written dirs (<10s) are skipped");
}
