//! Tests `cgn admin index` writes to ~/.gnx/<repo>/<branch>/ and updates registry.
//! (replaces the old `cgn analyze` top-level command, folded into admin)

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
            "git@github.com:E-NoR/routing-test.git",
        ])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
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
fn analyze_writes_to_registry_resolved_path() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let home_tmp = tempfile::tempdir().unwrap();
    init_repo(repo_tmp.path());

    let out = Command::new(gnx_bin())
        .args([
            "admin",
            "index",
            "--repo",
            repo_tmp.path().to_str().unwrap(),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .expect("cgn spawn failed");

    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // v2 layout: ~/.gnx/<repo>__<hash8>/commits/<source_type>_<source_id>__<sha>/graph.bin
    let gnx_root = home_tmp.path().join(".gnx");
    let entries: Vec<_> = walkdir::WalkDir::new(&gnx_root)
        .max_depth(4)
        .into_iter()
        .filter_map(Result::ok)
        .collect();

    let graph_bin = entries.iter().find(|e| e.file_name() == "graph.bin");
    assert!(
        graph_bin.is_some(),
        "graph.bin missing under {:?}; entries: {:?}",
        gnx_root,
        entries.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
    let commit_dir = graph_bin.unwrap().path().parent().unwrap();
    assert!(
        commit_dir.join("meta.json").exists(),
        "commit meta.json missing at {:?}",
        commit_dir
    );

    // Per-repo RepoMeta (v2): ~/.gnx/<repo>__<hash>/meta.json
    let repo_dir = commit_dir.parent().unwrap().parent().unwrap();
    assert!(
        repo_dir.join("meta.json").exists(),
        "repo meta.json missing at {:?}",
        repo_dir
    );
}
