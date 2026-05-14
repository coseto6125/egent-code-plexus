use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/rename-test.git",
        ])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::write(path.join("x"), "x").unwrap();
    Command::new("git")
        .args(["add", "x"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-c", "user.email=t@t",
            "-c", "user.name=t",
            "commit", "-q", "-m", "i",
        ])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn rename_branch_moves_index_dir() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    init_repo(repo_tmp.path());

    // Pre-create a fake index for the "old" branch
    let from_dir = home_tmp.path().join(".gnx/rename-test/old-name");
    std::fs::create_dir_all(&from_dir).unwrap();
    std::fs::write(from_dir.join("graph.bin"), b"data").unwrap();

    let to_dir = home_tmp.path().join(".gnx/rename-test/new-name");

    let out = Command::new(gnx_bin())
        .args([
            "rename-branch",
            "--from=old-name",
            "--to=new-name",
            &format!("--repo={}", repo_tmp.path().display()),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rename-branch failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!from_dir.exists(), "expected from_dir to be removed");
    assert!(to_dir.exists(), "expected to_dir to exist after rename");
    assert!(to_dir.join("graph.bin").exists(), "expected graph.bin in to_dir");
}

#[test]
fn rename_branch_no_op_when_from_dir_missing() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    init_repo(repo_tmp.path());

    // No from_dir — should succeed without error
    let out = Command::new(gnx_bin())
        .args([
            "rename-branch",
            "--from=ghost",
            "--to=new-name",
            &format!("--repo={}", repo_tmp.path().display()),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rename-branch (no-op) failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
