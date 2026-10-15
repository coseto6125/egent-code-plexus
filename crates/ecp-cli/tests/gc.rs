use ecp_cli::admin::gc::{
    enforce_quota, reachability, sweep_retired_repos, sweep_sessions, sweep_stale_generations,
};
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
        agent_name: None,
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
        agent_name: None,
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
        pid: None,
        started_at: old.to_rfc3339(),
        last_touched: old.to_rfc3339(),
        base_sha: "0".repeat(40),
        source_worktree: "/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
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
fn sweep_stale_generations_skips_when_sha_has_active_building() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "e".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.500.5.0"));
    // REAL append-style building marker for this SHA's base dir (no .gen suffix),
    // matching orchestrator.rs:682 `branch_main__{sha}.building`.
    std::fs::create_dir_all(commits.join(format!("branch_main__{sha}.building"))).unwrap();

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path()).unwrap();
    assert_eq!(
        stats.removed, 0,
        "no gen deleted while a build for this SHA is active"
    );
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

#[test]
fn sweep_retired_repos_removes_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path();
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123")).unwrap();
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123.dead.111.0.1700000000000")).unwrap();
    std::fs::create_dir_all(home_ecp.join("other__def456.dead.222.1.1700000000001")).unwrap();

    let stats = sweep_retired_repos(home_ecp).unwrap();

    assert_eq!(stats.removed, 2, "both .dead.* repo dirs removed");
    assert!(
        home_ecp.join("myrepo__abc123").exists(),
        "live repo untouched"
    );
    assert!(!home_ecp
        .join("myrepo__abc123.dead.111.0.1700000000000")
        .exists());
    assert!(!home_ecp
        .join("other__def456.dead.222.1.1700000000001")
        .exists());
}

/// The pid in session_meta belongs to the one-shot `ecp` process that enrolled
/// the session, so it is dead within milliseconds. Sweeping on that probe
/// marked every live session dead on the first pass (FU-2026-06-10-cc120f78889c).
#[test]
fn sweep_sessions_keeps_recently_touched_session_with_dead_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path();
    let sessions = repo_root.join("sessions").join("live-sid");
    std::fs::create_dir_all(&sessions).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let sm = SessionMeta {
        version: 1,
        session_id: "live-sid".into(),
        pid: Some(999_999_999),
        started_at: now.clone(),
        last_touched: now,
        base_sha: "0".repeat(40),
        source_worktree: "/x".into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&sessions.join("session_meta.json"), &sm).unwrap();

    let stats = sweep_sessions(repo_root).unwrap();
    assert_eq!(stats.marked, 0, "an active session must survive the sweep");
    assert!(sessions.exists());
}

/// `ecp admin gc` removes the private git index of a worktree that is gone
/// and keeps the one of a worktree that still exists.
#[cfg(target_os = "linux")]
#[test]
fn test_admin_gc_private_index_of_deleted_worktree_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("ecp-home");
    let index_dir = |name: &str, gitdir: &std::path::Path| {
        let dir = home.join("git-index").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index-1-2-3-00"), b"copy").unwrap();
        std::fs::write(dir.join("gitdir"), gitdir.to_str().unwrap()).unwrap();
        dir
    };
    let orphan = index_dir("0123456789abcdef", &tmp.path().join("deleted/.git"));
    let live = index_dir("fedcba9876543210", tmp.path());

    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "gc"])
        .env("ECP_HOME", &home)
        .env("HOME", tmp.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!orphan.exists(), "orphan private index survived gc");
    assert!(live.join("index-1-2-3-00").exists());
}

/// Every release and every `BUILDER_FINGERPRINT` bump opens a new
/// `parse_cache/<fp>/` dir; the old ones are never read again. Measured on
/// 2026-10-15: 173 such dirs, 831 MB under one `~/.ecp`.
fn parse_cache_dir(repo_root: &std::path::Path, name: &str, idle_days: u64) -> std::path::PathBuf {
    let dir = repo_root.join("parse_cache").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("0000000000000000.rkyv"), b"x").unwrap();
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(idle_days * 86_400);
    filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(then)).unwrap();
    dir
}

#[test]
fn test_sweep_stale_parse_caches_idle_foreign_fingerprint_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(tmp.path(), "deadbeef", 8);
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!stale.exists(), "idle foreign fingerprint dir survived gc");
}

/// Another installed ecp version may still write its own fingerprint dir;
/// sweeping it on every gc would make the two binaries evict each other.
#[test]
fn test_sweep_stale_parse_caches_recent_foreign_fingerprint_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let recent = parse_cache_dir(tmp.path(), "cafef00d", 1);
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(recent.exists());
}

#[test]
fn test_sweep_stale_parse_caches_current_fingerprint_kept_however_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let current = parse_cache_dir(
        tmp.path(),
        ecp_cli::parse_cache::ParseCache::current_dir_name(),
        400,
    );
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(current.exists());
}

#[test]
fn test_sweep_stale_parse_caches_no_parse_cache_dir_removes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0);
}

/// A short-lived CLI can exit before `retire_dir_async`'s background delete
/// finishes, leaving `<fp>.dead.<pid>.<n>.<ms>` behind.
#[test]
fn test_sweep_stale_parse_caches_retired_leftover_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let leftover = parse_cache_dir(tmp.path(), "deadbeef.dead.1.2.3", 0);
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!leftover.exists());
}

#[test]
fn test_sweep_stale_parse_caches_stray_file_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("parse_cache");
    std::fs::create_dir_all(&cache).unwrap();
    let stray = cache.join("notes.txt");
    std::fs::write(&stray, b"x").unwrap();
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path()).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(stray.exists());
}

/// A repo whose commit graphs gc may judge: `~/.ecp/<repo>/meta.json` points
/// at the work tree's `.git`, and `commits/` holds one graph per SHA.
struct CommitFixture {
    _tmp: tempfile::TempDir,
    wt: std::path::PathBuf,
    repo_root: std::path::PathBuf,
}

impl CommitFixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        git_init_with_commit(&wt);
        let repo_root = tmp.path().join("repo_root");
        std::fs::create_dir_all(repo_root.join("commits")).unwrap();
        let meta = ecp_core::registry::RepoMeta {
            version: 1,
            common_dir: wt.join(".git").to_string_lossy().into(),
            remote_url: None,
            aliases: vec![],
            known_refs: Default::default(),
            last_built_sha: None,
            total_size_bytes: 0,
            last_touched: chrono::Utc::now().to_rfc3339(),
        };
        ecp_core::registry::RepoMeta::write_atomic(&repo_root.join("meta.json"), &meta).unwrap();
        Self {
            _tmp: tmp,
            wt,
            repo_root,
        }
    }

    fn commit(&self, file: &str) -> String {
        std::fs::write(self.wt.join(file), file).unwrap();
        for args in [vec!["add", "."], vec!["commit", "-qm", file]] {
            Command::new("git")
                .arg("-C")
                .arg(&self.wt)
                .args(&args)
                .status()
                .unwrap();
        }
        let o = Command::new("git")
            .arg("-C")
            .arg(&self.wt)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    }

    fn graph(&self, sha: &str, built_days_ago: i64) -> std::path::PathBuf {
        let built = chrono::Utc::now() - chrono::Duration::days(built_days_ago);
        make_commit_dir(&self.repo_root.join("commits"), sha, &built.to_rfc3339(), 8);
        let dir = self
            .repo_root
            .join("commits")
            .join(format!("commit__{sha}"));
        let then = std::time::SystemTime::now()
            - std::time::Duration::from_secs(built_days_ago.max(0) as u64 * 86_400);
        filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(then)).unwrap();
        dir
    }

    fn sweep(&self) -> ecp_cli::admin::gc::SweepStats {
        ecp_cli::admin::gc::sweep_unreachable_commits(&self.repo_root).unwrap()
    }
}

/// A merged branch or a deleted worktree leaves every graph it built behind;
/// before this sweep nothing reclaimed them below the 5 GiB quota (measured
/// 2026-10-15: 245 of 252 graphs, 1.2 GB, at no live ref or HEAD).
#[test]
fn test_sweep_unreachable_commits_old_graph_at_no_ref_removed() {
    let fx = CommitFixture::new();
    let orphan = fx.graph(&"a".repeat(40), 30);
    let stats = fx.sweep();
    assert_eq!(stats.removed, 1);
    assert!(!orphan.exists());
}

#[test]
fn test_sweep_unreachable_commits_recent_unreachable_graph_kept() {
    let fx = CommitFixture::new();
    let recent = fx.graph(&"a".repeat(40), 1);
    assert_eq!(fx.sweep().removed, 0);
    assert!(recent.exists());
}

#[test]
fn test_sweep_unreachable_commits_branch_tip_graph_kept_however_old() {
    let fx = CommitFixture::new();
    let tip = fx.commit("b");
    let live = fx.graph(&tip, 400);
    assert_eq!(fx.sweep().removed, 0);
    assert!(live.exists());
}

/// A detached worktree HEAD is not a ref; `for-each-ref` alone misses it.
#[test]
fn test_sweep_unreachable_commits_detached_worktree_head_kept() {
    let fx = CommitFixture::new();
    let detached = fx.commit("b");
    fx.commit("c");
    let other = fx.wt.parent().unwrap().join("detached");
    Command::new("git")
        .arg("-C")
        .arg(&fx.wt)
        .args(["worktree", "add", "-q", "--detach"])
        .arg(&other)
        .arg(&detached)
        .status()
        .unwrap();
    let live = fx.graph(&detached, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(live.exists());
}

#[test]
fn test_sweep_unreachable_commits_active_session_base_kept() {
    let fx = CommitFixture::new();
    let base = "b".repeat(40);
    let sessions = fx.repo_root.join("sessions").join("sid1");
    std::fs::create_dir_all(&sessions).unwrap();
    let sm = SessionMeta {
        version: 1,
        session_id: "sid1".into(),
        pid: None,
        started_at: "2026-05-17T10:00:00Z".into(),
        last_touched: chrono::Utc::now().to_rfc3339(),
        base_sha: base.clone(),
        source_worktree: fx.wt.to_string_lossy().into(),
        overlay_version: 0,
        watcher_pid: None,
        last_drained_offset: 0,
        agent_name: None,
    };
    SessionMeta::write_atomic(&sessions.join("session_meta.json"), &sm).unwrap();
    let live = fx.graph(&base, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(live.exists());
}

/// No git answer means no proof a graph is unreachable: an empty live set
/// must never read as "everything is garbage".
#[test]
fn test_sweep_unreachable_commits_missing_git_dir_removes_nothing() {
    let fx = CommitFixture::new();
    let graph = fx.graph(&"a".repeat(40), 30);
    std::fs::remove_dir_all(fx.wt.join(".git")).unwrap();
    assert_eq!(fx.sweep().removed, 0);
    assert!(graph.exists());
}

#[test]
fn test_sweep_unreachable_commits_missing_repo_meta_removes_nothing() {
    let fx = CommitFixture::new();
    let graph = fx.graph(&"a".repeat(40), 30);
    std::fs::remove_file(fx.repo_root.join("meta.json")).unwrap();
    assert_eq!(fx.sweep().removed, 0);
    assert!(graph.exists());
}

#[test]
fn test_sweep_unreachable_commits_building_dir_kept() {
    let fx = CommitFixture::new();
    let sha = "a".repeat(40);
    fx.graph(&sha, 30);
    let building = fx
        .repo_root
        .join("commits")
        .join(format!("commit__{sha}.building"));
    std::fs::create_dir_all(&building).unwrap();
    fx.sweep();
    assert!(building.exists(), "an in-progress build dir was swept");
}

/// `retire_dir_async` hands the delete to a thread that dies with the
/// short-lived gc process; the next sweep must finish the job.
#[test]
fn test_sweep_unreachable_commits_retired_leftover_removed() {
    let fx = CommitFixture::new();
    let leftover = fx
        .repo_root
        .join("commits")
        .join(format!("commit__{}.dead.1.2.3", "a".repeat(40)));
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("graph.bin"), b"x").unwrap();
    assert_eq!(fx.sweep().removed, 1);
    assert!(!leftover.exists());
}

/// SessionStart, the first query after an upgrade, `ecp update` and a manual
/// run can all start gc; a second one must back off instead of racing the
/// first to retire the same dirs.
#[test]
fn test_admin_gc_lock_held_skips_run() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    let _held = ecp_core::registry::FileLock::try_exclusive(&home.path().join(".gc.lock")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "gc"])
        .env("ECP_HOME", home.path())
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("another gc is running"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(stale.exists(), "a locked-out gc still deleted");
}

#[test]
fn test_admin_gc_idle_foreign_parse_cache_removed() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "gc"])
        .env("ECP_HOME", home.path())
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stale.exists(),
        "end-to-end gc left an idle foreign parse cache"
    );
}

fn run_gc(home: &std::path::Path, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "gc"])
        .args(extra)
        .env("ECP_HOME", home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stamp(home: &std::path::Path, hours_ago: u64) {
    let path = home.join(".gc-last");
    std::fs::write(&path, b"").unwrap();
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(hours_ago * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(then)).unwrap();
}

/// Each run spends two git calls per indexed repo, and the sweeps judge on a
/// 7-day grace: SessionStart (every Claude session) must not repeat it.
#[test]
fn test_admin_gc_auto_recent_run_skips() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    stamp(home.path(), 1);
    assert!(run_gc(home.path(), &["--auto"]).contains("skipped"));
    assert!(stale.exists());
}

#[test]
fn test_admin_gc_auto_old_run_sweeps() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    stamp(home.path(), 13);
    run_gc(home.path(), &["--auto"]);
    assert!(!stale.exists());
}

/// An upgrade trigger or a manual run omits `--auto` and ignores the stamp.
#[test]
fn test_admin_gc_without_auto_ignores_recent_stamp() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    stamp(home.path(), 1);
    run_gc(home.path(), &[]);
    assert!(!stale.exists());
}

#[test]
fn test_admin_gc_run_writes_stamp_dry_run_does_not() {
    let home = tempfile::tempdir().unwrap();
    run_gc(home.path(), &["--dry-run"]);
    assert!(!home.path().join(".gc-last").exists());
    run_gc(home.path(), &[]);
    assert!(home.path().join(".gc-last").exists());
}
