use std::process::Command;

fn gnx_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("cgn")
}

fn git_init(p: &std::path::Path) -> String {
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["init", "-q"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.email", "t@t"])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["config", "user.name", "t"])
        .status()
        .unwrap();
    std::fs::write(p.join("hello.rs"), "fn hello() {}").unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["commit", "-qm", "init"])
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
fn admin_index_without_force_builds_when_l2_absent() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("l2.built"),
        "expected l2.built in stderr: {stderr}"
    );
}

#[test]
fn admin_index_without_force_skips_when_l2_exists() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .status()
        .unwrap();
    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("l2.exists"),
        "expected l2.exists in stderr: {stderr}"
    );
    assert!(stderr.contains("--force to rebuild"));
}

#[test]
fn admin_index_with_force_rebuilds_existing_l2() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .status()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let out = Command::new(gnx_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .arg("--force")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("l2.rebuilt"),
        "expected l2.rebuilt: {stderr}"
    );
}

#[test]
fn admin_index_rejects_no_cache_flag() {
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--no-cache"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "--no-cache should be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--no-cache"),
        "expected clap rejection: {stderr}"
    );
}

#[test]
fn admin_index_rejects_embeddings_flag() {
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--embeddings"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "--embeddings should be rejected");
}

#[test]
fn admin_index_rejects_drop_embeddings_flag() {
    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "/tmp/x", "--drop-embeddings"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--drop-embeddings should be rejected"
    );
}
