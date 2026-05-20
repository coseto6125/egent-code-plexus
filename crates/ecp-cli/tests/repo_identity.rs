use ecp_cli::repo_identity::repo_dir_name_for_cwd;
use std::process::Command;

#[test]
fn cwd_in_git_repo_returns_basename_hash() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    let name = repo_dir_name_for_cwd(tmp.path()).unwrap();
    assert!(name.contains("__"), "must contain hash separator: {name}");
    let (prefix, hash) = name.rsplit_once("__").unwrap();
    assert!(!prefix.is_empty(), "basename prefix must be non-empty");
    assert_eq!(hash.len(), 8, "hash suffix must be 8 hex chars: {name}");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "hash must be hex: {hash}"
    );
}

#[test]
fn two_worktrees_same_repo_yield_same_dir_name() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    std::fs::create_dir(&primary).unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&primary)
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    std::fs::write(primary.join("README"), "x").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&primary)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&primary)
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
    let wt = tmp.path().join("wt2");
    Command::new("git")
        .arg("-C")
        .arg(&primary)
        .args(["worktree", "add", "-q", wt.to_str().unwrap()])
        .status()
        .unwrap();

    let n1 = repo_dir_name_for_cwd(&primary).unwrap();
    let n2 = repo_dir_name_for_cwd(&wt).unwrap();
    assert_eq!(n1, n2, "two worktrees of same repo must share dir name");
}

#[test]
fn cwd_not_in_repo_errors() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(repo_dir_name_for_cwd(tmp.path()).is_err());
}

#[test]
fn name_format_is_safe_chars_only() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    let name = repo_dir_name_for_cwd(tmp.path()).unwrap();
    // sanitize_segment whitelist: alnum + `_` + `.` + `-`; plus `__` separator
    assert!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')),
        "name must be fs-safe: {name}"
    );
}
