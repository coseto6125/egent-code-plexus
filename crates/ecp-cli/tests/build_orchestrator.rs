// End-to-end build test — requires Task 4.4 to carve out
// `commands::admin::index::run_analyzer_for_paths`. Marked #[ignore]
// until 4.4 lands; the test code is correct as-is.

#[test]
#[ignore = "requires Task 4.4 — run_analyzer_for_paths extraction"]
fn first_build_writes_commit_dir_atomically() {
    use ecp_cli::build::orchestrator;
    use std::process::Command;

    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    std::fs::write(worktree.join("main.rs"), "fn main() { println!(\"hi\"); }").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(&worktree)
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

    let home = tmp.path().join("home");
    std::env::set_var("HOME", &home);

    let result = orchestrator::build_l2(&worktree, None).unwrap();
    assert!(
        result.commit_dir.exists(),
        "commit dir must exist: {:?}",
        result.commit_dir
    );
    assert!(result.commit_dir.join("graph.bin").exists());
    assert!(result.commit_dir.join("meta.json").exists());
    let building = result.commit_dir.with_extension("building");
    assert!(
        !building.exists(),
        "building suffix must be gone after atomic rename"
    );
}
