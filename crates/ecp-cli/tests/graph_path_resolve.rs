use ecp_cli::graph_path;
use std::path::Path;
use std::process::Command;

const LEGACY_DEFAULT: &str = ".ecp/graph.bin";

#[test]
fn custom_path_passes_through() {
    let custom = Path::new("/abs/custom/graph.bin");
    let cwd = Path::new("/tmp");
    let resolved = graph_path::resolve(custom, cwd);
    assert_eq!(resolved, std::path::PathBuf::from("/abs/custom/graph.bin"));
}

#[test]
fn non_default_relative_path_passes_through() {
    let rel = Path::new("custom/foo.bin");
    let cwd = Path::new("/tmp");
    let resolved = graph_path::resolve(rel, cwd);
    assert_eq!(resolved, std::path::PathBuf::from("custom/foo.bin"));
}

#[test]
fn legacy_default_not_in_git_repo_falls_back_to_input() {
    let tmp = tempfile::tempdir().unwrap(); // no git init
    let resolved = graph_path::resolve(Path::new(LEGACY_DEFAULT), tmp.path());
    // Fall through verbatim — caller's "graph.bin not found" surfaces
    assert_eq!(resolved, std::path::PathBuf::from(LEGACY_DEFAULT));
}

#[test]
#[ignore = "requires HOME env + actual L2 build; enable after Phase 4"]
fn legacy_default_in_git_repo_resolves_to_commits_dir() {
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    std::fs::write(tmp.path().join("a"), "x").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
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

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());

    let resolved = graph_path::resolve(Path::new(LEGACY_DEFAULT), tmp.path());
    let s = resolved.to_string_lossy();
    assert!(s.contains(".ecp"), "got: {s}");
    assert!(s.contains("commits"), "got: {s}");
    assert!(s.ends_with("graph.bin"), "got: {s}");
}

#[test]
fn legacy_default_in_git_repo_without_l2_falls_back_to_input() {
    // git repo exists, but no <home>/.ecp/<repo>/commits/<sha>/ entry yet
    // (cold start: first run before any build)
    let tmp = tempfile::tempdir().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    std::fs::write(tmp.path().join("a"), "x").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(tmp.path())
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

    // Point HOME to an empty dir → no commits index exists
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", home.path());

    let resolved = graph_path::resolve(Path::new(LEGACY_DEFAULT), tmp.path());
    // Falls back to input — caller's auto_ensure will trigger build on this miss
    assert_eq!(resolved, std::path::PathBuf::from(LEGACY_DEFAULT));
}
