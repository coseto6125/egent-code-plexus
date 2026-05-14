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
            "git@github.com:E-NoR/index-test.git",
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
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "i",
        ])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn index_registers_existing_dot_gitnexus_rs_into_registry() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    init_repo(repo_tmp.path());

    // Pre-create a bare `.gitnexus-rs/graph.bin` to simulate a crashed analyze
    let index_dir = repo_tmp.path().join(".gitnexus-rs");
    std::fs::create_dir_all(&index_dir).unwrap();
    std::fs::write(index_dir.join("graph.bin"), b"").unwrap();

    let out = Command::new(gnx_bin())
        .args(["index", &repo_tmp.path().display().to_string()])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let list = Command::new(gnx_bin())
        .args(["list", "--format=json"])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "list failed: stderr={}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("index-test"),
        "expected repo 'index-test' in registry list, got: {stdout}"
    );
}

#[test]
fn index_rejects_path_without_dot_gitnexus_rs() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    // No .gitnexus-rs/ at all
    let out = Command::new(gnx_bin())
        .args(["index", &repo_tmp.path().display().to_string()])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit when .gitnexus-rs/graph.bin missing"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no .gitnexus-rs/graph.bin"),
        "expected stderr to mention missing .gitnexus-rs/graph.bin, got: {stderr}"
    );
}
