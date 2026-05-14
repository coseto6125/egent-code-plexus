//! Tests `gnx analyze-here` indexes the current working directory.

use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn init_repo(path: &Path) {
    Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/analyze-here-test.git",
        ])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::write(path.join("foo.py"), "def foo():\n    return 1\n").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
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
            "init",
        ])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn analyze_here_indexes_cwd() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_repo(repo_tmp.path());

    let out = Command::new(gnx_bin())
        .arg("analyze-here")
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("gnx spawn failed");

    assert!(
        out.status.success(),
        "analyze-here failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let index_dir = home_tmp.path().join(".gnx/analyze-here-test/main");
    assert!(
        index_dir.join("graph.bin").exists(),
        "graph.bin missing at {:?}",
        index_dir
    );
}
