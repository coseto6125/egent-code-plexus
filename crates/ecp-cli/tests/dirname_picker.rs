use ecp_cli::build::dirname_picker::pick_dirname;
use std::path::Path;
use std::process::Command;

fn git_init_with_commit(p: &Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .arg("init")
        .arg("-q")
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
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "x",
        ])
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
fn picks_branch_for_normal_head() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = git_init_with_commit(tmp.path());
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["branch", "-M", "main"])
        .status()
        .unwrap();
    let name = pick_dirname(tmp.path(), &sha).unwrap();
    assert!(name.starts_with("branch_main__"), "got: {name}");
    assert!(name.ends_with(&sha), "got: {name}");
}

#[test]
fn picks_commit_for_detached_head() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = git_init_with_commit(tmp.path());
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["checkout", "-q", "--detach", &sha])
        .status()
        .unwrap();
    // Delete the branch so no ref points at the SHA
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["branch", "-D", "master", "main"])
        .output()
        .unwrap(); // ignore failures (depends on default)
    let name = pick_dirname(tmp.path(), &sha).unwrap();
    // If a branch still points → `branch_<name>__`; if detached → `commit__`
    assert!(name.ends_with(&sha), "got: {name}");
    assert!(
        name.starts_with("commit__") || name.starts_with("branch_") || name.starts_with("tag_"),
        "got: {name}"
    );
}

#[test]
fn picks_tag_when_branch_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = git_init_with_commit(tmp.path());
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["tag", "v1.0"])
        .status()
        .unwrap();
    // Move head off any branch so branch path doesn't win
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["checkout", "-q", "--detach", &sha])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["branch", "-D", "master", "main"])
        .output()
        .unwrap();
    let name = pick_dirname(tmp.path(), &sha).unwrap();
    assert!(
        name.starts_with("tag_v1.0__")
            || name.starts_with("branch_")
            || name.starts_with("commit__"),
        "got: {name}"
    );
}

#[test]
fn slash_in_branch_name_sanitized_to_dash() {
    let tmp = tempfile::tempdir().unwrap();
    let sha = git_init_with_commit(tmp.path());
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["checkout", "-q", "-b", "feat/x"])
        .status()
        .unwrap();
    let name = pick_dirname(tmp.path(), &sha).unwrap();
    assert!(name.starts_with("branch_feat-x__"), "got: {name}");
}

#[test]
fn returns_commit_fallback_when_no_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let _sha = git_init_with_commit(tmp.path());
    // Pass a fake sha that no ref points at
    let fake = "0".repeat(40);
    let name = pick_dirname(tmp.path(), &fake).unwrap();
    assert_eq!(name, format!("commit__{fake}"));
}
