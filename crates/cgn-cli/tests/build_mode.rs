use cgn_cli::build::mode::{build_mode, BuildMode};

#[test]
fn first_build_is_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = [0u8; 20];
    assert_eq!(build_mode(tmp.path(), &sha), BuildMode::Sync);
}

#[test]
fn target_exists_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    let dir = "branch_main__abc123def4567890abc123def4567890abc123de";
    std::fs::create_dir(commits.join(dir)).unwrap();
    let mut sha = [0u8; 20];
    hex::decode_to_slice("abc123def4567890abc123def4567890abc123de", &mut sha).unwrap();
    assert_eq!(build_mode(tmp.path(), &sha), BuildMode::None);
}

#[test]
fn other_commits_exist_target_missing_is_background() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("branch_main__abc123def4567890abc123def4567890abc123de"))
        .unwrap();
    let mut other = [0u8; 20];
    hex::decode_to_slice("000000000000000000000000000000000000fffe", &mut other).unwrap();
    assert_eq!(build_mode(tmp.path(), &other), BuildMode::Background);
}

#[test]
fn building_suffix_excluded_so_only_inflight_is_treated_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let commits = tmp.path().join("commits");
    std::fs::create_dir(&commits).unwrap();
    std::fs::create_dir(commits.join("branch_x__abc.building")).unwrap();
    let sha = [1u8; 20];
    // Only the in-flight dir exists → treated as empty → Sync
    assert_eq!(build_mode(tmp.path(), &sha), BuildMode::Sync);
}
