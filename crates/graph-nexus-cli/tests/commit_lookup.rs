use graph_nexus_cli::commit_lookup::CommitIndex;

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
    let dir1 = "branch_main__abc123def4567890abc123def4567890abc123de";
    let dir2 = "tag_v1.0__def456789abc123def456789abc123def456789ab";
    let dir3 = "commit__789abc123def456789abc123def456789abc1234a";
    std::fs::create_dir(commits.join(dir1)).unwrap();
    std::fs::create_dir(commits.join(dir2)).unwrap();
    std::fs::create_dir(commits.join(dir3)).unwrap();

    let idx = CommitIndex::scan(&commits).unwrap();
    assert_eq!(idx.len(), 3);

    let mut sha2 = [0u8; 20];
    hex::decode_to_slice("def456789abc123def456789abc123def456789ab", &mut sha2).unwrap();
    assert_eq!(idx.find(&sha2), Some(dir2));
}
