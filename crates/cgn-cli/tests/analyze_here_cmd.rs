//! Tests `cgn admin index --repo .` indexes the current working directory.
//! (replaces the old `cgn analyze-here` top-level command, folded into admin)

use std::path::Path;
use std::process::Command;

fn cgn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cgn")
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

    let out = Command::new(cgn_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo_tmp.path())
        .env("HOME", home_tmp.path())
        .output()
        .expect("cgn spawn failed");

    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // v2 layout: ~/.cgn/<repo>__<hash8>/commits/<source_type>_<source_id>__<sha>/graph.bin
    // Walk ~/.cgn/*/commits/*/graph.bin and assert at least one exists.
    let cgn_root = home_tmp.path().join(".cgn");
    let found = walkdir::WalkDir::new(&cgn_root)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_name() == "graph.bin");
    assert!(found, "graph.bin missing under {:?}", cgn_root);
}
