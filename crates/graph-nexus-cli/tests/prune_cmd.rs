use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

#[test]
fn prune_removes_index_dir_and_registry_branch() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    // Init git repo
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo_tmp.path())
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/prune-test.git",
        ])
        .current_dir(repo_tmp.path())
        .output()
        .unwrap();
    std::fs::write(repo_tmp.path().join("x"), "x").unwrap();
    Command::new("git")
        .args(["add", "x"])
        .current_dir(repo_tmp.path())
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "i",
        ])
        .current_dir(repo_tmp.path())
        .output()
        .unwrap();

    // Pre-create a fake index for a branch
    let branch_dir = home_tmp.path().join(".gnx/prune-test/feat-x");
    std::fs::create_dir_all(&branch_dir).unwrap();
    std::fs::write(branch_dir.join("graph.bin"), b"junk").unwrap();

    let out = Command::new(gnx_bin())
        .args([
            "admin",
            "prune",
            "--branch",
            "feat-x",
            "--repo",
            &repo_tmp.path().display().to_string(),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin prune failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!branch_dir.exists(), "expected branch dir to be removed");
}

#[test]
fn prune_orphans_drops_entries_with_missing_worktree_path() {
    use graph_nexus_core::registry::{RegistryFile, RepoEntry};

    let home_tmp = tempfile::tempdir().unwrap();
    let valid_wt = tempfile::tempdir().unwrap();
    let home_gnx = home_tmp.path().join(".gnx");
    std::fs::create_dir_all(&home_gnx).unwrap();

    let valid_index = home_gnx.join("valid-repo");
    let orphan_index = home_gnx.join("orphan-repo");
    std::fs::create_dir_all(valid_index.join("main")).unwrap();
    std::fs::create_dir_all(orphan_index.join("main")).unwrap();
    std::fs::write(orphan_index.join("main/graph.bin"), b"junk").unwrap();

    let registry = RegistryFile {
        version: 1,
        repos: vec![
            RepoEntry {
                name: "valid-repo".into(),
                remote_url: "git@github.com:E-NoR/valid.git".into(),
                worktree_path: valid_wt.path().display().to_string(),
                index_dir_root: valid_index.display().to_string(),
                branches: vec![],
                groups: vec![],
            },
            RepoEntry {
                name: "orphan-repo".into(),
                remote_url: "git@github.com:E-NoR/orphan.git".into(),
                worktree_path: "/nonexistent/path/that/does/not/exist".into(),
                index_dir_root: orphan_index.display().to_string(),
                branches: vec![],
                groups: vec![],
            },
        ],
        groups: vec![],
    };
    let registry_path = home_gnx.join("registry.json");
    std::fs::write(
        &registry_path,
        serde_json::to_string(&registry).unwrap(),
    )
    .unwrap();

    let out = Command::new(gnx_bin())
        .args(["admin", "prune", "--orphans"])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin prune --orphans failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !orphan_index.exists(),
        "orphan repo's index dir should be removed"
    );
    assert!(
        valid_index.exists(),
        "valid repo's index dir should remain"
    );

    let updated: RegistryFile =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).unwrap()).unwrap();
    assert_eq!(
        updated.repos.len(),
        1,
        "expected one repo to remain after orphan sweep"
    );
    assert_eq!(updated.repos[0].name, "valid-repo");
}
