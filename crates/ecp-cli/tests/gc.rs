use ecp_cli::admin::gc::{sweep_retired_dirs, sweep_sessions, sweep_stale_generations};
use ecp_core::registry::{CommitBuildMeta, EmbeddingStatus, FileLock, SourceType};
use ecp_core::session::SessionMeta;
use std::process::Command;

/// Runs git in `dir` and returns its stdout, failing the test on a non-zero
/// exit. The developer's global and system config stay out (a
/// `commit.gpgsign` or a hooks path would make a commit fail or differ), and
/// so does a `GIT_DIR` inherited from a git hook that runs the tests.
fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
    git_out(dir, args);
}

fn git_init_with_commit(p: &std::path::Path) -> String {
    git_in(p, &["init", "-q"]);
    std::fs::write(p.join("a"), "x").unwrap();
    git_in(p, &["add", "."]);
    git_in(p, &["commit", "-qm", "x"]);
    git_out(p, &["rev-parse", "HEAD"])
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

    let stats = sweep_sessions(repo_root, false).unwrap();
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

    let stats = sweep_sessions(repo_root, false).unwrap();
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

    let stats = sweep_stale_generations(tmp.path(), false).unwrap();

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

/// An older ecp builds in `<name>.building/` and holds `.build.lock` inside
/// it until just before its rename. Returns the held lock. The dir is fresh,
/// which is what gc goes by; it never probes that lock.
fn hold_legacy_build(commits: &std::path::Path, name: &str) -> FileLock {
    let building = commits.join(format!("{name}.building"));
    std::fs::create_dir_all(&building).unwrap();
    FileLock::acquire_exclusive(&building.join(".build.lock")).unwrap()
}

/// Contract: while an older ecp may be mid-build for a SHA (its legacy
/// staging dir was written within the 7-day grace), gc converges none of
/// that SHA's generations. This one stages under a generation-shaped name.
#[test]
fn sweep_stale_generations_skips_building() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "c".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    let older = format!("branch_main__{sha}.gen.500.5.0");
    make_named_commit_dir(&commits, &older);
    let _held = hold_legacy_build(&commits, &older);

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path(), false).unwrap();
    assert_eq!(
        stats.removed, 0,
        "older dir guarded by a recent legacy build dir must stay"
    );
    assert!(commits.join(&older).exists());
}

/// Same contract as `sweep_stale_generations_skips_building`, for the
/// base-shaped `<base>.building` that older ecp versions wrote.
#[test]
fn sweep_stale_generations_skips_when_sha_has_active_building() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "e".repeat(40);
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.1000.10.0"));
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.500.5.0"));
    let _held = hold_legacy_build(&commits, &format!("branch_main__{sha}"));

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path(), false).unwrap();
    assert_eq!(
        stats.removed, 0,
        "no gen deleted while a build for this SHA is active"
    );
}

/// A `.building` dir a crashed older ecp left used to block the SHA's
/// convergence for ever. Once nothing wrote to it for the 7-day grace, the
/// SHA converges, and the leftover itself stays: gc never removes old-style
/// staging (doctor reports it).
#[test]
fn test_sweep_stale_generations_idle_legacy_leftover_converges_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "f".repeat(40);
    let old = commits.join(format!("branch_main__{sha}.gen.500.5.0"));
    let new = commits.join(format!("branch_main__{sha}.gen.1000.10.0"));
    make_named_commit_dir(&commits, old.file_name().unwrap().to_str().unwrap());
    make_named_commit_dir(&commits, new.file_name().unwrap().to_str().unwrap());
    let leftover = commits.join(format!("branch_main__{sha}.building"));
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("graph.bin"), b"half").unwrap();
    let idle = filetime::FileTime::from_system_time(
        std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400),
    );
    filetime::set_file_mtime(leftover.join("graph.bin"), idle).unwrap();
    filetime::set_file_mtime(&leftover, idle).unwrap();

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path(), false).unwrap();
    let staging = ecp_cli::admin::gc::sweep_abandoned_staging(tmp.path(), false).unwrap();

    assert_eq!(stats.removed, 1);
    assert!(!old.exists() && new.exists());
    assert_eq!(staging.marked, 0);
    assert!(leftover.join("graph.bin").exists());
}

/// Readers of an old-binary layout (two bare names of one SHA under two
/// refs, equal generation) pick by mtime; gc keeps that same dir.
#[test]
fn test_sweep_stale_generations_bare_tie_keeps_scan_choice() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "9".repeat(40);
    let older = format!("branch_a__{sha}");
    let newer = format!("branch_b__{sha}");
    make_named_commit_dir(&commits, &older);
    make_named_commit_dir(&commits, &newer);
    let at = |secs| filetime::FileTime::from_unix_time(secs, 0);
    filetime::set_file_mtime(commits.join(&older).join("graph.bin"), at(1_000_000_000)).unwrap();
    filetime::set_file_mtime(commits.join(&newer).join("graph.bin"), at(1_000_000_500)).unwrap();
    let mut sha_bytes = [0u8; 20];
    hex::decode_to_slice(&sha, &mut sha_bytes).unwrap();
    let chosen = ecp_cli::commit_lookup::CommitIndex::scan(&commits)
        .unwrap()
        .find(&sha_bytes)
        .map(str::to_string);
    assert_eq!(chosen.as_deref(), Some(newer.as_str()));

    let stats = sweep_stale_generations(tmp.path(), false).unwrap();

    assert_eq!(stats.removed, 1);
    assert!(commits.join(&newer).exists() && !commits.join(&older).exists());
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

    let stats = ecp_cli::admin::gc::sweep_stale_generations(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0, "freshly-written dirs (<10s) are skipped");
}

#[test]
fn test_sweep_retired_dirs_dead_repo_dirs_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path();
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123")).unwrap();
    std::fs::create_dir_all(home_ecp.join("myrepo__abc123.dead.111.0.1700000000000")).unwrap();
    std::fs::create_dir_all(home_ecp.join("other__def456.dead.222.1.1700000000001")).unwrap();

    let stats = sweep_retired_dirs(home_ecp, false).unwrap();

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

    let stats = sweep_sessions(repo_root, false).unwrap();
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
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!stale.exists(), "idle foreign fingerprint dir survived gc");
}

/// Another installed ecp version may still write its own fingerprint dir;
/// sweeping it on every gc would make the two binaries evict each other.
#[test]
fn test_sweep_stale_parse_caches_recent_foreign_fingerprint_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let recent = parse_cache_dir(tmp.path(), "cafef00d", 1);
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
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
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(current.exists());
}

#[test]
fn test_sweep_stale_parse_caches_no_parse_cache_dir_removes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
}

/// gc's own retire renames before it removes; a remove that fails leaves
/// `<fp>.dead.<pid>.<n>.<ms>` behind for the next run.
#[test]
fn test_sweep_stale_parse_caches_retired_leftover_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let leftover = parse_cache_dir(tmp.path(), "deadbeef.dead.1.2.3", 0);
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
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
    let stats = ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false).unwrap();
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
        git_in(&self.wt, &["add", "."]);
        git_in(&self.wt, &["commit", "-qm", file]);
        git_out(&self.wt, &["rev-parse", "HEAD"])
    }

    fn graph(&self, sha: &str, built_days_ago: u64) -> std::path::PathBuf {
        self.graph_aged(sha, built_days_ago, built_days_ago)
    }

    /// A graph whose `built_at` and dir mtime differ: an attach writes into a
    /// dir long after its build.
    fn graph_aged(
        &self,
        sha: &str,
        built_days_ago: u64,
        mtime_days_ago: u64,
    ) -> std::path::PathBuf {
        let days = |n: u64| std::time::Duration::from_secs(n * 86_400);
        let built = chrono::Utc::now() - chrono::Duration::from_std(days(built_days_ago)).unwrap();
        make_commit_dir(&self.repo_root.join("commits"), sha, &built.to_rfc3339(), 8);
        let dir = self
            .repo_root
            .join("commits")
            .join(format!("commit__{sha}"));
        age_commit_dir(&dir, std::time::SystemTime::now() - days(mtime_days_ago));
        dir
    }

    fn sweep(&self) -> ecp_cli::admin::gc::SweepStats {
        ecp_cli::admin::gc::sweep_unreachable_commits(&self.repo_root, false).unwrap()
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

/// The 7-day grace is on `built_at`; a dir whose mtime alone is old (the
/// build wrote nothing since) is not old enough.
#[test]
fn test_sweep_unreachable_commits_recently_built_old_mtime_graph_kept() {
    let fx = CommitFixture::new();
    let recent = fx.graph_aged(&"a".repeat(40), 1, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(recent.exists());
}

/// A builder holds the SHA lock from its existence check through its
/// publish; gc must not retire the SHA's graph inside that span.
#[test]
fn test_sweep_unreachable_commits_sha_lock_held_kept() {
    let fx = CommitFixture::new();
    let sha = "a".repeat(40);
    let orphan = fx.graph(&sha, 30);
    let mut sha_bytes = [0u8; 20];
    hex::decode_to_slice(&sha, &mut sha_bytes).unwrap();
    let lock = ecp_cli::commit_lookup::sha_lock_path(&fx.repo_root.join("commits"), &sha_bytes);
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    let held = FileLock::acquire_exclusive(&lock).unwrap();
    assert_eq!(fx.sweep().removed, 0);
    assert!(orphan.exists());
    drop(held);
    assert_eq!(fx.sweep().removed, 1);
}

/// An attach back-fills sidecars into a dir built long ago; that recent write
/// marks the graph as in use, whatever `built_at` says.
#[test]
fn test_sweep_unreachable_commits_recently_modified_old_build_graph_kept() {
    let fx = CommitFixture::new();
    let attached = fx.graph_aged(&"a".repeat(40), 30, 1);
    assert_eq!(fx.sweep().removed, 0);
    assert!(attached.exists());
}

/// A branch that is not checked out anywhere is live through its ref alone:
/// no worktree HEAD and no HEAD parent names it.
#[test]
fn test_sweep_unreachable_commits_unchecked_out_branch_tip_kept() {
    let fx = CommitFixture::new();
    let side = fx.commit("b");
    git_in(&fx.wt, &["branch", "side"]);
    fx.commit("c");
    fx.commit("d");
    let live = fx.graph(&side, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(live.exists());
}

/// A stray file in `sessions/` (`.DS_Store`, an editor backup) holds no
/// session; it must not read as an unreadable session meta, which would stop
/// the sweep for the repo on every run.
#[test]
fn test_sweep_unreachable_commits_stray_file_in_sessions_still_sweeps() {
    let fx = CommitFixture::new();
    let orphan = fx.graph(&"a".repeat(40), 30);
    std::fs::create_dir_all(fx.repo_root.join("sessions")).unwrap();
    std::fs::write(fx.repo_root.join("sessions").join(".DS_Store"), b"x").unwrap();
    assert_eq!(fx.sweep().removed, 1);
    assert!(!orphan.exists());
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
    git_in(
        &fx.wt,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            other.to_str().unwrap(),
            &detached,
        ],
    );
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

/// Contract: gc never removes old-style staging (`<name>.building`) on its
/// own, however old its mtime and its `built_at`; an older ecp may still own
/// it, and `ecp admin doctor` reports it instead. The generation-shaped name
/// parses as a commit dir, so the name filter alone keeps it.
#[test]
fn test_sweep_unreachable_commits_building_dir_kept() {
    let fx = CommitFixture::new();
    let sha = "a".repeat(40);
    let commits = fx.repo_root.join("commits");
    let mut kept = Vec::new();
    for name in [
        format!("commit__{sha}.building"),
        format!("commit__{sha}.gen.1000.10.0.building"),
    ] {
        let graph = fx.graph(&sha, 30);
        let building = commits.join(name);
        std::fs::rename(&graph, &building).unwrap();
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
        filetime::set_file_mtime(&building, filetime::FileTime::from_system_time(then)).unwrap();
        kept.push(building);
    }
    assert_eq!(fx.sweep().removed, 0);
    for building in kept {
        assert!(building.exists(), "an in-progress build dir was swept");
    }
}

/// `retire_dir_async` hands the delete to a thread that dies with the
/// short-lived CLI process; the next sweep must finish the job.
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
    stamp_by(home, hours_ago, ecp_core::registry::BUILDER_FINGERPRINT);
}

fn stamp_by(home: &std::path::Path, hours_ago: u64, fingerprint: &str) {
    let path = home.join(".gc-last");
    std::fs::write(&path, fingerprint).unwrap();
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(hours_ago * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(then)).unwrap();
}

/// The sweeps judge on a 7-day grace, so a repeat within hours finds
/// nothing: SessionStart (every Claude session) must not run it again.
#[test]
fn test_admin_gc_auto_recent_run_skips() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    stamp(home.path(), 1);
    assert!(run_gc(home.path(), &["--auto"]).contains("skipped"));
    assert!(stale.exists());
}

/// A run by another installed version does not throttle this one: the first
/// automatic trigger after an upgrade must run once.
#[test]
fn test_admin_gc_auto_recent_run_by_other_version_sweeps() {
    let home = tempfile::tempdir().unwrap();
    let stale = parse_cache_dir(&home.path().join("repo__00000000"), "deadbeef", 30);
    stamp_by(home.path(), 1, "v0.0.0+schema0");
    run_gc(home.path(), &["--auto"]);
    assert!(!stale.exists());
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
    assert_eq!(
        std::fs::read_to_string(home.path().join(".gc-last")).unwrap(),
        ecp_core::registry::BUILDER_FINGERPRINT
    );
}

/// `admin index --force` retires a repo's whole `parse_cache/` and deletes it
/// on a thread that dies with the process; the leftover sits beside
/// `parse_cache/`, not inside it.
#[test]
fn test_admin_gc_repo_root_retired_parse_cache_removed() {
    let home = tempfile::tempdir().unwrap();
    let repo_root = home.path().join("repo__00000000");
    let cache = repo_root.join("parse_cache").join("deadbeef");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("0000000000000000.rkyv"), b"x").unwrap();
    let retired = ecp_core::registry::retire_dir(&repo_root.join("parse_cache"))
        .unwrap()
        .unwrap();
    let stray_file = repo_root.join("notes.dead.1.2.3");
    std::fs::write(&stray_file, b"x").unwrap();
    run_gc(home.path(), &[]);
    assert!(!retired.exists(), "retired parse cache survived gc");
    assert!(stray_file.exists(), "a file is not a retired dir");
}

/// `%(objectname)` of an annotated tag is the tag object, not the commit
/// the graph dir is keyed by; the peeled target must count as live.
#[test]
fn test_sweep_unreachable_commits_annotated_tag_target_kept() {
    let fx = CommitFixture::new();
    let tagged = fx.commit("b");
    git_in(&fx.wt, &["tag", "-a", "v1", "-m", "v1"]);
    fx.commit("c");
    fx.commit("d");
    let live = fx.graph(&tagged, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(live.exists());
}

/// Warm-attach serves a HEAD that has no graph yet from its parent's graph,
/// so that graph must outlive its branch moving on.
#[test]
fn test_sweep_unreachable_commits_worktree_head_parent_kept() {
    let fx = CommitFixture::new();
    let parent = fx.commit("b");
    fx.commit("c");
    let warm = fx.graph(&parent, 30);
    assert_eq!(fx.sweep().removed, 0);
    assert!(warm.exists());
}

/// An existing git dir whose git command fails (not a repo) must keep every
/// graph: a failed call's empty output is not "nothing is live".
#[test]
fn test_sweep_unreachable_commits_git_command_failure_removes_nothing() {
    let fx = CommitFixture::new();
    let graph = fx.graph(&"a".repeat(40), 30);
    let not_git = fx.wt.parent().unwrap().join("not-git");
    std::fs::create_dir_all(&not_git).unwrap();
    let mut meta = ecp_core::registry::RepoMeta::read(&fx.repo_root.join("meta.json")).unwrap();
    meta.common_dir = not_git.to_string_lossy().into();
    ecp_core::registry::RepoMeta::write_atomic(&fx.repo_root.join("meta.json"), &meta).unwrap();
    assert_eq!(fx.sweep().removed, 0);
    assert!(graph.exists());
}

/// A session meta that exists but cannot be read may name a live base: the
/// sweep must not trust the partial session list.
#[cfg(unix)]
#[test]
fn test_sweep_unreachable_commits_unreadable_session_meta_removes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let fx = CommitFixture::new();
    let graph = fx.graph(&"a".repeat(40), 30);
    let session = fx.repo_root.join("sessions").join("sid1");
    std::fs::create_dir_all(&session).unwrap();
    let meta = session.join("session_meta.json");
    std::fs::write(&meta, b"{}").unwrap();
    std::fs::set_permissions(&meta, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&meta).is_ok() {
        eprintln!("skipped: running with permission override (root)");
        return;
    }
    let removed = fx.sweep().removed;
    std::fs::set_permissions(&meta, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(removed, 0);
    assert!(graph.exists());
}

/// An inherited GIT_COMMON_DIR overrides `--git-dir` for `worktree list`; the
/// detached HEAD of this repo must still be seen.
#[test]
fn test_admin_gc_inherited_git_common_dir_keeps_detached_head() {
    let fx = CommitFixture::new();
    let detached = fx.commit("b");
    fx.commit("c");
    fx.commit("d");
    let other = fx.wt.parent().unwrap().join("detached");
    git_in(
        &fx.wt,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            other.to_str().unwrap(),
            &detached,
        ],
    );
    let live = fx.graph(&detached, 30);
    let foreign = tempfile::tempdir().unwrap();
    git_init_with_commit(foreign.path());
    let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["admin", "gc"])
        .env("ECP_HOME", fx.repo_root.parent().unwrap())
        .env("ECP_SKIP_BG_REBUILD", "1")
        .env("GIT_COMMON_DIR", foreign.path().join(".git"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        live.exists(),
        "detached HEAD graph swept under a foreign GIT_COMMON_DIR"
    );
}

/// A cache read without writes leaves the dir's mtime old; the stamp
/// `ParseCache::open` rewrites marks the use.
#[test]
fn test_sweep_stale_parse_caches_recently_opened_old_dir_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = parse_cache_dir(tmp.path(), "cafef00d", 30);
    std::fs::write(dir.join(ecp_cli::parse_cache::LAST_USED_STAMP), b"").unwrap();
    filetime::set_file_mtime(
        &dir,
        filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400),
        ),
    )
    .unwrap();
    assert_eq!(
        ecp_cli::admin::gc::sweep_stale_parse_caches(tmp.path(), false)
            .unwrap()
            .removed,
        0
    );
    assert!(dir.exists());
}

#[test]
fn test_parse_cache_open_writes_last_used_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    ecp_cli::parse_cache::ParseCache::open(tmp.path()).unwrap();
    let stamp = tmp
        .path()
        .join("parse_cache")
        .join(ecp_cli::parse_cache::ParseCache::current_dir_name())
        .join(ecp_cli::parse_cache::LAST_USED_STAMP);
    assert!(stamp.exists());
}

/// A relative ECP_HOME made the background job's lock and markers relative
/// to the job's own cwd, so the job never ran. SessionStart's detached
/// `admin prune --orphans` must finish and drop its marker under the home.
#[test]
fn test_session_start_relative_ecp_home_background_job_completes() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let envelope = format!(r#"{{"cwd": {:?}}}"#, tmp.path().to_string_lossy());
    let mut child = Command::new(env!("CARGO_BIN_EXE_ecp"))
        .args(["hook", "session-start", "--claude-code"])
        .current_dir(tmp.path())
        .env("ECP_HOME", "home")
        .env("HOME", tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(envelope.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let done = |name: &str| home.join(name).exists();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !(done(".prune-complete") || done(".prune-failed")) {
        assert!(
            std::time::Instant::now() < deadline,
            "background prune never finished under a relative ECP_HOME"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        !home.join("home").exists(),
        "job resolved home against its own cwd"
    );
}

/// A binary upgrade is when the old caches become garbage: the first query
/// that finds its graph built by another fingerprint starts gc in the
/// background, although another version's gc ran within the throttle window.
/// `ecp update` (the other upgrade trigger) needs the network; not tested.
#[test]
fn test_query_after_fingerprint_change_starts_gc() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    std::fs::create_dir(&wt).unwrap();
    git_init_with_commit(&wt);
    std::fs::write(wt.join("lib.rs"), "pub fn upgrade_probe() {}\n").unwrap();
    git_in(&wt, &["add", "."]);
    git_in(&wt, &["commit", "-qm", "lib"]);
    let home = tmp.path().join("home");
    let ecp = |args: &[&str], skip_bg: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ecp"));
        cmd.args(args)
            .env("ECP_HOME", &home)
            .env("HOME", tmp.path())
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("ECP_SKIP_BG_REBUILD");
        if skip_bg {
            cmd.env("ECP_SKIP_BG_REBUILD", "1");
        }
        cmd.output().unwrap()
    };
    let indexed = ecp(&["admin", "index", "--repo", wt.to_str().unwrap()], true);
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let mut graphs = 0;
    for repo in std::fs::read_dir(&home).unwrap().flatten() {
        let Ok(commits) = std::fs::read_dir(repo.path().join("commits")) else {
            continue;
        };
        for dir in commits.flatten() {
            let graph = dir.path().join("graph.bin");
            if graph.is_file() {
                std::fs::write(
                    ecp_cli::auto_ensure::builder_fingerprint_sidecar_path(&graph),
                    "v0.0.0+schema0",
                )
                .unwrap();
                graphs += 1;
            }
        }
    }
    assert!(graphs > 0, "admin index published no graph");
    let stale = parse_cache_dir(&home.join("other__00000000"), "deadbeef", 30);
    stamp_by(&home, 1, "v0.0.0+schema0");

    let query = ecp(
        &["find", "upgrade_probe", "--repo", wt.to_str().unwrap()],
        false,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while stale.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "no gc ran after the first query on a drifted graph; query stderr: {}",
            String::from_utf8_lossy(&query.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

const GRAPH_WRITTEN_BY_0_14_8: &[u8] = include_bytes!("fixtures/graph-written-by-0.14.8.bin");

/// A commit dir holding `graph`, with the format sidecar the build writes
/// when `sidecar` names a version, untouched for `idle_days`.
fn format_commit_dir(
    repo_root: &std::path::Path,
    name: &str,
    graph: &[u8],
    sidecar: Option<u32>,
    idle_days: u64,
) -> std::path::PathBuf {
    let dir = repo_root.join("commits").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("graph.bin"), graph).unwrap();
    if let Some(version) = sidecar {
        std::fs::write(
            dir.join("graph.bin.compatible_version"),
            format!("{version}\n"),
        )
        .unwrap();
    }
    age_commit_dir(
        &dir,
        std::time::SystemTime::now() - std::time::Duration::from_secs(idle_days * 86_400),
    );
    dir
}

/// Sets the commit dir's mtime and its `graph.bin`'s atime to `then`: a
/// fresh write leaves the atime at now, which reads as a query's use.
fn age_commit_dir(dir: &std::path::Path, then: std::time::SystemTime) {
    let then = filetime::FileTime::from_system_time(then);
    filetime::set_file_atime(dir.join("graph.bin"), then).unwrap();
    filetime::set_file_mtime(dir, then).unwrap();
}

fn current_format_graph() -> Vec<u8> {
    let mut fx = ecp_core::graph_fixture::GraphFixture::new();
    fx.func("a.py", "f");
    fx.into_bytes()
}

/// The graph ecp 0.14.8 wrote, with the sidecar it writes beside it.
#[test]
fn test_sweep_old_format_commits_idle_v12_graph_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let old = format_commit_dir(
        tmp.path(),
        "branch_main__1111",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        8,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!((stats.marked, stats.removed), (1, 1));
    assert!(!old.exists(), "an idle v12 graph survived gc");
}

/// A graph whose detached sidecar write was lost is judged by its header.
#[test]
fn test_sweep_old_format_commits_v12_graph_without_sidecar_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let old = format_commit_dir(
        tmp.path(),
        "branch_main__2222",
        GRAPH_WRITTEN_BY_0_14_8,
        None,
        8,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!old.exists());
}

/// A second installed ecp version may still read its own graphs.
#[test]
fn test_sweep_old_format_commits_recent_v12_graph_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let recent = format_commit_dir(
        tmp.path(),
        "branch_main__3333",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        1,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(recent.exists());
}

#[test]
fn test_sweep_old_format_commits_current_format_graph_kept_however_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let current = ecp_core::graph::GRAPH_FORMAT_VERSION;
    let with_sidecar = format_commit_dir(
        tmp.path(),
        "branch_main__4444",
        &current_format_graph(),
        Some(current),
        400,
    );
    let without_sidecar = format_commit_dir(
        tmp.path(),
        "branch_main__5555",
        &current_format_graph(),
        None,
        400,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(with_sidecar.exists() && without_sidecar.exists());
}

#[test]
fn test_sweep_old_format_commits_building_dir_and_no_commits_dir_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    let building = format_commit_dir(
        tmp.path(),
        "branch_main__6666.building",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        30,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(building.exists());
}

/// End to end: `ecp admin gc` runs the sweep for every repo under its home.
#[test]
fn test_admin_gc_idle_v12_commit_removed() {
    let home = tempfile::tempdir().unwrap();
    let old = format_commit_dir(
        &home.path().join("repo__00000000"),
        "branch_main__7777",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        30,
    );
    run_gc(home.path(), &[]);
    assert!(!old.exists(), "end-to-end gc left an idle v12 graph");
}

fn backdate_secs(path: &std::path::Path, secs: u64) {
    let then = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(then)).unwrap();
}

/// `sessions/<sid>/` with `manifest` as `l1/dirty_events.json` (written as is
/// when given) and the named bins in `l1/fragments/`, each `age_secs` old.
/// Returns the session directory and its fragments directory.
fn fragment_session(
    repo_root: &std::path::Path,
    manifest: Option<&str>,
    bins: &[(&str, u64)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let session = repo_root.join("sessions").join("sid");
    let fragments = ecp_core::session::layout::fragments_dir(&session);
    std::fs::create_dir_all(&fragments).unwrap();
    if let Some(manifest) = manifest {
        std::fs::write(
            ecp_core::session::layout::dirty_events_path(&session),
            manifest,
        )
        .unwrap();
    }
    write_aged_bins(&fragments, bins);
    (session, fragments)
}

/// [`fragment_session`] for the legacy namespace: `manifest` as
/// `dirty_files.json`, the bins in `graph_overlay/`.
fn legacy_fragment_session(
    repo_root: &std::path::Path,
    manifest: Option<&str>,
    bins: &[(&str, u64)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let session = repo_root.join("sessions").join("sid");
    let overlay = ecp_core::session::layout::legacy_overlay_dir(&session);
    std::fs::create_dir_all(&overlay).unwrap();
    if let Some(manifest) = manifest {
        std::fs::write(
            ecp_core::session::layout::legacy_dirty_files_path(&session),
            manifest,
        )
        .unwrap();
    }
    write_aged_bins(&overlay, bins);
    (session, overlay)
}

fn write_aged_bins(dir: &std::path::Path, bins: &[(&str, u64)]) {
    for (name, age_secs) in bins {
        let bin = dir.join(name);
        std::fs::write(&bin, b"x").unwrap();
        backdate_secs(&bin, *age_secs);
    }
}

/// A `dirty_events.json` whose one entry, `a.py`, names `fragment_id`.
fn manifest_naming(fragment_id: &str) -> String {
    use ecp_core::contracts::dirty_event::{PathChange, PathClass};
    use ecp_core::session::{DirtyEventEntry, DirtyEvents};
    let mut manifest = DirtyEvents::empty(ecp_core::contracts::routing::SizeCap::BUILT_IN);
    manifest.entries.insert(
        "a.py".into(),
        DirtyEventEntry {
            change: PathChange::Modified,
            class: PathClass::SOURCE,
            mtime_ns: 1,
            fragment_id: Some(fragment_id.into()),
            content_hash: Some(fragment_id.into()),
            parse_failed: false,
            dirty_symbols: vec![],
            format: ecp_core::session::FRAGMENT_FORMAT_V5,
        },
    );
    serde_json::to_string(&manifest).unwrap()
}

/// A legacy `dirty_files.json` whose one entry, `a.py`, names `fragment_id`.
fn legacy_manifest_naming(fragment_id: &str) -> String {
    format!(
        r#"{{"version":1,"entries":{{"a.py":{{"mtime_ns":1,"content_hash":"{fragment_id}","fragment_id":"{fragment_id}","tantivy_delta_segment":null,"format":3}}}}}}"#
    )
}

#[test]
fn test_sweep_stale_fragments_settled_orphans_removed_named_and_young_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, overlay) = fragment_session(
        tmp.path(),
        Some(manifest_naming("aaaa").as_str()),
        &[
            ("aaaa.bin", 7200),
            ("bbbb.bin", 7200),
            ("cccc.bin", 0),
            ("dddd.1.2.tmp", 7200),
            ("notes.txt", 7200),
        ],
    );
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 2);
    assert!(overlay.join("aaaa.bin").exists(), "a named bin was removed");
    assert!(
        !overlay.join("bbbb.bin").exists(),
        "a settled orphan survived"
    );
    assert!(
        overlay.join("cccc.bin").exists(),
        "a bin a writer may not have committed yet was removed"
    );
    assert!(!overlay.join("dddd.1.2.tmp").exists());
    assert!(overlay.join("notes.txt").exists());
}

#[test]
fn test_sweep_stale_fragments_unreadable_manifest_keeps_every_bin() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, overlay) = fragment_session(tmp.path(), Some("not json"), &[("bbbb.bin", 7200)]);
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(overlay.join("bbbb.bin").exists());
}

#[test]
fn test_sweep_stale_fragments_idle_session_dry_run_matches_real_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    let (session, overlay) = fragment_session(tmp.path(), None, &[("bbbb.bin", 7200)]);
    let old = chrono::Utc::now() - chrono::Duration::hours(48);
    let sm = SessionMeta {
        version: 1,
        session_id: "sid".into(),
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
    SessionMeta::write_atomic(&session.join("session_meta.json"), &sm).unwrap();
    assert_eq!(sweep_sessions(tmp.path(), true).unwrap().marked, 1);
    let dry = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), true).unwrap();
    assert!(overlay.join("bbbb.bin").exists());
    assert_eq!(sweep_sessions(tmp.path(), false).unwrap().marked, 1);
    let real = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(real.removed, 0);
    assert_eq!(dry.would_remove.len(), real.removed);
}

/// The legacy namespace is swept against its own manifest: a bin the legacy
/// `dirty_files.json` names stays, a settled orphan beside it goes.
#[test]
fn test_sweep_stale_fragments_legacy_namespace_orphan_removed_named_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, overlay) = legacy_fragment_session(
        tmp.path(),
        Some(legacy_manifest_naming("aaaa").as_str()),
        &[("aaaa.bin", 7200), ("bbbb.bin", 7200)],
    );
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(overlay.join("aaaa.bin").exists(), "a named bin was removed");
    assert!(
        !overlay.join("bbbb.bin").exists(),
        "a settled orphan survived"
    );
}

/// The session ecp 0.14.8 left: its legacy manifest names its bin, so the bin
/// stays. This binary's refresh writes `l1/` and never touches the legacy
/// namespace, so the old manifest and bin outlive the refresh. They go whole
/// once nothing has written the namespace for a day.
#[test]
fn test_sweep_stale_fragments_session_written_by_0_14_8_legacy_namespace_retired_when_idle() {
    use ecp_core::session::layout;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/overlay-written-by-0.14.8");
    let tmp = tempfile::tempdir().unwrap();
    let old_bin = "553e94275e6085db.bin";
    let (session, overlay) = legacy_fragment_session(
        tmp.path(),
        Some(
            std::fs::read_to_string(fixture.join("dirty_files.json"))
                .unwrap()
                .as_str(),
        ),
        &[],
    );
    std::fs::copy(
        fixture.join("graph_overlay").join(old_bin),
        overlay.join(old_bin),
    )
    .unwrap();
    backdate_secs(&overlay.join(old_bin), 7200);
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0, "a bin its manifest names was removed");

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
    let fresh = ecp_core::analyzer::types::LocalGraph {
        file_path: "other.py".into(),
        content_hash: [9; 8],
        ..Default::default()
    };
    ecp_cli::session::overlay_writer::write_dirty_fragments_from_graphs(&session, &[fresh], &[1])
        .unwrap();
    let manifest =
        ecp_core::session::DirtyEvents::read(&layout::dirty_events_path(&session)).unwrap();
    assert!(manifest.from_current_builder());
    assert_eq!(
        manifest.entries.keys().collect::<Vec<_>>(),
        ["other.py"],
        "the 0.14.8 entry leaked into today's manifest"
    );
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0, "a namespace written today was retired");
    assert!(
        overlay.join(old_bin).exists(),
        "the refresh replaced the legacy manifest"
    );

    let two_days = 48 * 3600;
    backdate_secs(&layout::legacy_dirty_files_path(&session), two_days);
    backdate_secs(&overlay, two_days);
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 2, "the manifest and the bins directory");
    assert!(!overlay.exists());
    assert!(!layout::legacy_dirty_files_path(&session).exists());
    let kept = std::fs::read_dir(layout::fragments_dir(&session))
        .unwrap()
        .count();
    assert_eq!(kept, 1, "the retired legacy namespace took today's bin");
}

/// A writer holds the overlay lock while it re-publishes a content-addressed
/// bin and commits the manifest naming it; a gc that judged the old copy
/// orphan before that would delete the new one.
#[test]
fn test_sweep_stale_fragments_overlay_lock_held_session_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let (session, overlay) = fragment_session(
        tmp.path(),
        Some(manifest_naming("aaaa").as_str()),
        &[("bbbb.bin", 7200)],
    );
    let held = ecp_core::registry::FileLock::try_exclusive(
        &ecp_cli::session::overlay_writer::overlay_lock_path(&session),
    )
    .unwrap();
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(
        overlay.join("bbbb.bin").exists(),
        "gc deleted a bin while a writer held the session"
    );
    drop(held);
    let stats = ecp_cli::admin::gc::sweep_stale_fragments(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 1, "a released session was not swept");
    assert!(!overlay.join("bbbb.bin").exists());
}

fn graph_with_version(version: u32) -> Vec<u8> {
    let mut fx = ecp_core::graph_fixture::GraphFixture::new();
    fx.func("a.py", "f");
    let mut graph = fx.build();
    graph.version = version;
    rkyv::to_bytes::<rkyv::rancor::Error>(&graph)
        .unwrap()
        .into_vec()
}

/// A newer ecp's graph fails this binary's header check exactly as an older
/// one does; with no sidecar only the version its header records tells them
/// apart, and the newer binary still reads it.
#[test]
fn test_sweep_old_format_commits_newer_format_graph_without_sidecar_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let newer = format_commit_dir(
        tmp.path(),
        "branch_main__8888",
        &graph_with_version(ecp_core::graph::GRAPH_FORMAT_VERSION + 1),
        None,
        30,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(newer.exists(), "a newer version's graph was deleted as old");
}

/// A torn write is no proof of an older layout.
#[test]
fn test_sweep_old_format_commits_unreadable_graph_without_sidecar_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let torn = format_commit_dir(tmp.path(), "branch_main__9999", b"torn write", None, 30);
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(torn.exists(), "an unreadable graph was deleted as old");
}

/// A query maps `graph.bin`, which refreshes its atime and leaves the dir's
/// mtime alone: a second installed version that queries daily keeps its graph.
#[test]
fn test_sweep_old_format_commits_recently_read_graph_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let read = format_commit_dir(
        tmp.path(),
        "branch_main__1010",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        30,
    );
    filetime::set_file_atime(read.join("graph.bin"), filetime::FileTime::now()).unwrap();
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(read.exists(), "a graph read today was deleted as idle");
}

#[test]
fn test_sweep_unreachable_commits_recently_read_graph_kept() {
    let fx = CommitFixture::new();
    let read = fx.graph(&"a".repeat(40), 30);
    filetime::set_file_atime(read.join("graph.bin"), filetime::FileTime::now()).unwrap();
    assert_eq!(fx.sweep().removed, 0);
    assert!(read.exists(), "a graph read today was deleted as idle");
}

/// gc's own header read must not count as a use: it runs on every idle
/// graph without a sidecar, and would keep each one alive for ever.
#[test]
fn test_sweep_old_format_commits_header_read_graph_atime_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let current = format_commit_dir(
        tmp.path(),
        "branch_main__1111",
        &current_format_graph(),
        None,
        400,
    );
    let graph = current.join("graph.bin");
    let before = filetime::FileTime::from_last_access_time(&std::fs::metadata(&graph).unwrap());
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 0);
    assert_eq!(
        filetime::FileTime::from_last_access_time(&std::fs::metadata(&graph).unwrap()),
        before,
        "gc's header read refreshed the graph's atime"
    );
}

/// `fix/cache.stale-check` puts `.stale` before the `__`; only a suffix after
/// it marks a recovery dir.
#[test]
fn test_sweep_unreachable_commits_branch_named_stale_removed() {
    let fx = CommitFixture::new();
    let sha = "a".repeat(40);
    let graph = fx.graph(&sha, 30);
    let named = fx
        .repo_root
        .join("commits")
        .join(format!("branch_fix_cache.stale-check__{sha}"));
    std::fs::rename(&graph, &named).unwrap();
    age_commit_dir(
        &named,
        std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400),
    );
    assert_eq!(fx.sweep().removed, 1);
    assert!(!named.exists(), "a branch named *.stale* was never swept");
}

#[test]
fn test_sweep_old_format_commits_branch_named_building_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let named = format_commit_dir(
        tmp.path(),
        "branch_feat_x.building-y__1212",
        GRAPH_WRITTEN_BY_0_14_8,
        Some(12),
        30,
    );
    let stats = ecp_cli::admin::gc::sweep_old_format_commits(tmp.path(), false).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(
        !named.exists(),
        "a branch named *.building* was never swept"
    );
}

/// `.last-gc` was the throttle stamp of the background gc that `.gc-last`
/// replaced; nothing writes or reads it any more.
#[test]
fn test_admin_gc_legacy_last_gc_stamp_removed_dry_run_kept() {
    let home = tempfile::tempdir().unwrap();
    let legacy = home.path().join(".last-gc");
    std::fs::write(&legacy, b"").unwrap();
    run_gc(home.path(), &["--dry-run"]);
    assert!(legacy.exists(), "a dry run deleted the stamp");
    run_gc(home.path(), &[]);
    assert!(!legacy.exists(), "the retired stamp outlived gc");
}

/// Directories only: `commits/` also holds the per-SHA lock files, which are
/// not generations.
fn gen_dirs(commits: &std::path::Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(commits)
        .map(|it| {
            it.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| e.file_name() != ecp_cli::commit_lookup::LOCKS_DIR)
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Contract: a dry run tells the user what a real run would do and leaves the
/// disk as it was. Fixture mirrors the field report: several stale same-SHA
/// generations beside one current one.
#[test]
fn test_sweep_stale_generations_dry_run_stale_generations_listed_and_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "a".repeat(40);
    for gen in [
        "1000.10.0",
        "2000.20.0",
        "3000.30.0",
        "4000.40.0",
        "5000.50.0",
    ] {
        make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.{gen}"));
    }
    let before = gen_dirs(&commits);

    let stats = sweep_stale_generations(tmp.path(), true).unwrap();

    assert_eq!(stats.would_remove.len(), 4, "four stale generations named");
    assert_eq!(stats.removed, 0, "a dry run removes nothing");
    assert_eq!(gen_dirs(&commits), before, "every dir stays on disk");
    assert!(
        !stats
            .would_remove
            .iter()
            .any(|p| p.ends_with(format!("branch_main__{sha}.gen.5000.50.0"))),
        "the newest generation is never listed"
    );
}

#[test]
fn test_sweep_stale_generations_dry_run_matches_real_run() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "b".repeat(40);
    for gen in ["1000.10.0", "2000.20.0", "3000.30.0"] {
        make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.{gen}"));
    }

    let dry = sweep_stale_generations(tmp.path(), true).unwrap();
    let real = sweep_stale_generations(tmp.path(), false).unwrap();

    assert_eq!(dry.would_remove.len(), real.removed);
    assert!(dry.would_remove.iter().all(|p| !p.exists()));
}

#[test]
fn test_sweep_stale_generations_dry_run_clean_home_reports_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    make_named_commit_dir(
        &commits,
        &format!("branch_main__{}.gen.1000.10.0", "a".repeat(40)),
    );
    make_named_commit_dir(
        &commits,
        &format!("branch_main__{}.gen.1000.10.0", "b".repeat(40)),
    );

    let stats = sweep_stale_generations(tmp.path(), true).unwrap();
    assert!(stats.would_remove.is_empty());
    assert_eq!(stats.removed, 0);
}

#[test]
fn test_sweep_stale_generations_dry_run_missing_or_empty_home_reports_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = sweep_stale_generations(&tmp.path().join("no-such-home"), true).unwrap();
    assert!(missing.would_remove.is_empty());

    std::fs::create_dir_all(tmp.path().join("commits")).unwrap();
    let empty = sweep_stale_generations(tmp.path(), true).unwrap();
    assert!(empty.would_remove.is_empty());
}

#[test]
fn test_sweep_stale_generations_dry_run_building_and_fresh_dirs_not_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let building_sha = "c".repeat(40);
    make_named_commit_dir(
        &commits,
        &format!("branch_main__{building_sha}.gen.1000.10.0"),
    );
    make_named_commit_dir(
        &commits,
        &format!("branch_main__{building_sha}.gen.2000.20.0"),
    );
    // Contract: a dry run applies the eligibility of a real run, and a
    // recent legacy build dir is what keeps a SHA from converging.
    let _held = hold_legacy_build(&commits, &format!("branch_main__{building_sha}"));
    let fresh_sha = "d".repeat(40);
    for gen in ["1000.10.0", "2000.20.0"] {
        make_named_commit_dir(&commits, &format!("branch_main__{fresh_sha}.gen.{gen}"));
        filetime::set_file_mtime(
            commits.join(format!("branch_main__{fresh_sha}.gen.{gen}")),
            filetime::FileTime::now(),
        )
        .unwrap();
    }

    let stats = sweep_stale_generations(tmp.path(), true).unwrap();
    assert!(
        stats.would_remove.is_empty(),
        "the dry run applies the same guards as the real run"
    );
}

/// A forced rebuild publishes a dir whose mtime is seconds old. The in-flight
/// guard used to hide it, so its predecessor was a group of one and stayed.
#[test]
fn test_sweep_generations_keeping_fresh_published_dir_removes_predecessor_keeps_published() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "e".repeat(40);
    let old = commits.join(format!("branch_main__{sha}.gen.1000.10.0"));
    let published = commits.join(format!("branch_main__{sha}.gen.2000.20.0"));
    make_named_commit_dir(&commits, old.file_name().unwrap().to_str().unwrap());
    make_named_commit_dir(&commits, published.file_name().unwrap().to_str().unwrap());
    filetime::set_file_mtime(&published, filetime::FileTime::now()).unwrap();

    let plain = sweep_stale_generations(tmp.path(), true).unwrap();
    assert!(
        plain.would_remove.is_empty(),
        "without the published hint the fresh dir hides itself"
    );

    let stats =
        ecp_cli::admin::gc::sweep_generations_keeping(tmp.path(), false, Some(&published)).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!old.exists());
    assert!(published.exists(), "the published generation is kept");
}

/// Two forced rebuilds in a row (the second backdates nothing: the first
/// predecessor is aged by hand) leave one generation.
#[test]
fn test_sweep_generations_keeping_two_successive_publishes_leave_one_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "f".repeat(40);
    let name = |g: &str| format!("branch_main__{sha}.gen.{g}");
    make_named_commit_dir(&commits, &name("1000.10.0"));
    for g in ["2000.20.0", "3000.30.0"] {
        let published = commits.join(name(g));
        make_named_commit_dir(&commits, &name(g));
        filetime::set_file_mtime(&published, filetime::FileTime::now()).unwrap();
        // Back-to-back forced builds: the predecessor is seconds old too.
        ecp_cli::admin::gc::sweep_generations_keeping(tmp.path(), false, Some(&published)).unwrap();
    }
    assert_eq!(
        gen_dirs(&commits),
        std::iter::once(name("3000.30.0")).collect()
    );
}

/// The published dir wins even when a concurrent build left a higher
/// generation: both stay, nothing the caller just made is deleted.
#[test]
fn test_sweep_generations_keeping_published_below_newest_generation_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir_all(&commits).unwrap();
    let sha = "9".repeat(40);
    let published = commits.join(format!("branch_main__{sha}.gen.1000.10.0"));
    make_named_commit_dir(&commits, published.file_name().unwrap().to_str().unwrap());
    make_named_commit_dir(&commits, &format!("branch_main__{sha}.gen.2000.20.0"));

    let stats =
        ecp_cli::admin::gc::sweep_generations_keeping(tmp.path(), false, Some(&published)).unwrap();
    assert_eq!(stats.removed, 0);
    assert!(published.exists());
}
