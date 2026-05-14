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
            "prune",
            "--branch=feat-x",
            &format!("--repo={}", repo_tmp.path().display()),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "prune failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!branch_dir.exists(), "expected branch dir to be removed");
}
