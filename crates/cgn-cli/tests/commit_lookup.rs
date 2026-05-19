use cgn_cli::commit_lookup::CommitIndex;

#[test]
fn missing_commits_dir_returns_empty_index() {
    let tmp = tempfile::tempdir().unwrap();
    let idx = CommitIndex::scan(&tmp.path().join("does-not-exist")).unwrap();
    assert!(idx.is_empty());
    assert_eq!(idx.len(), 0);
    assert!(idx.find(&[0; 20]).is_none());
}

#[test]
fn empty_commits_dir_returns_empty_index() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let idx = CommitIndex::scan(&commits).unwrap();
    assert!(idx.is_empty());
    assert!(idx.find(&[0; 20]).is_none());
}

#[test]
fn finds_existing_commit_dir_by_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let dir = "branch_main__abc123def4567890abc123def4567890abc123de";
    std::fs::create_dir(commits.join(dir)).unwrap();
    let idx = CommitIndex::scan(&commits).unwrap();
    let mut sha = [0u8; 20];
    hex::decode_to_slice("abc123def4567890abc123def4567890abc123de", &mut sha).unwrap();
    assert_eq!(idx.find(&sha), Some(dir));
}

#[test]
fn scan_cached_matches_uncached_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let dir = "branch_main__abc123def4567890abc123def4567890abc123de";
    std::fs::create_dir(commits.join(dir)).unwrap();
    let cached = CommitIndex::scan_cached(&commits).unwrap();
    let uncached = CommitIndex::scan(&commits).unwrap();
    let mut sha = [0u8; 20];
    hex::decode_to_slice("abc123def4567890abc123def4567890abc123de", &mut sha).unwrap();
    assert_eq!(cached.find(&sha), uncached.find(&sha));
    assert_eq!(cached.len(), uncached.len());
}

#[test]
fn scan_cached_invalidates_on_mtime_bump() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let dir_a = "branch_a__abc123def4567890abc123def4567890abc123de";
    std::fs::create_dir(commits.join(dir_a)).unwrap();
    let first = CommitIndex::scan_cached(&commits).unwrap();
    assert_eq!(first.len(), 1);
    // Sleep so the dir mtime can roundtrip past filesystem timestamp granularity
    // (most filesystems carry second-resolution mtime; some sub-second).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let dir_b = "branch_b__11ee22dd33cc44bb55aa66998877665544332211";
    std::fs::create_dir(commits.join(dir_b)).unwrap();
    let second = CommitIndex::scan_cached(&commits).unwrap();
    assert_eq!(
        second.len(),
        2,
        "scan_cached must re-scan after mtime bump; got {} entries",
        second.len()
    );
}

#[test]
fn skips_unparseable_and_inflight_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    // garbage name — no __ separator
    std::fs::create_dir(commits.join("garbage_name")).unwrap();
    // in-flight build leftover
    std::fs::create_dir(
        commits.join("branch_x__abc123def4567890abc123def4567890abc123de.building"),
    )
    .unwrap();
    // promotion stale dir
    std::fs::create_dir(commits.join("branch_y.stale-abc123def4567890abc123def4567890abc123de"))
        .unwrap();
    let idx = CommitIndex::scan(&commits).unwrap();
    assert_eq!(
        idx.len(),
        0,
        "all 3 entries must be skipped, got: {} entries",
        idx.len()
    );
}

#[test]
fn handles_multiple_commit_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let sha1_hex = "abc123def4567890abc123def4567890abc123de";
    let sha2_hex = "def4567890abc123def4567890abc123def45678";
    let sha3_hex = "1234567890abcdef1234567890abcdef12345678";
    let dir1 = format!("branch_main__{sha1_hex}");
    let dir2 = format!("tag_v1.0__{sha2_hex}");
    let dir3 = format!("commit__{sha3_hex}");
    std::fs::create_dir(commits.join(&dir1)).unwrap();
    std::fs::create_dir(commits.join(&dir2)).unwrap();
    std::fs::create_dir(commits.join(&dir3)).unwrap();

    let idx = CommitIndex::scan(&commits).unwrap();
    assert_eq!(idx.len(), 3);

    let mut sha2 = [0u8; 20];
    hex::decode_to_slice(sha2_hex, &mut sha2).unwrap();
    assert_eq!(idx.find(&sha2), Some(dir2.as_str()));
}
