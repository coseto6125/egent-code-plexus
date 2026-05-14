//! Tests `gnx analyze` writes to ~/.gnx/<repo>/<branch>/ and updates registry.

use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str { env!("CARGO_BIN_EXE_gnx") }

fn init_repo(path: &Path) {
    Command::new("git").args(["init", "-q", "-b", "main"]).current_dir(path).output().unwrap();
    Command::new("git")
        .args(["remote", "add", "origin", "git@github.com:E-NoR/routing-test.git"])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
    Command::new("git").args(["add", "-A"]).current_dir(path).output().unwrap();
    Command::new("git")
        .args([
            "-c", "user.email=t@t",
            "-c", "user.name=t",
            "commit", "-q", "-m", "init",
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
        .args(["analyze", "--repo", repo_tmp.path().to_str().unwrap()])
        .env("HOME", home_tmp.path())
        .output()
        .expect("gnx spawn failed");

    assert!(
        out.status.success(),
        "analyze failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let index_dir = home_tmp.path().join(".gnx/routing-test/main");
    assert!(
        index_dir.exists(),
        "expected ~/.gnx/routing-test/main/ to exist; got listing of .gnx: {:?}",
        std::fs::read_dir(home_tmp.path().join(".gnx"))
            .map(|it| it.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .ok()
    );
    assert!(index_dir.join("graph.bin").exists(), "graph.bin missing");
    assert!(index_dir.join("meta.json").exists(), "meta.json missing");

    let registry_path = home_tmp.path().join(".gnx/registry.json");
    assert!(registry_path.exists());
    let registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    let repos = registry["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"], "routing-test");
    assert_eq!(repos[0]["branches"].as_array().unwrap().len(), 1);
    assert_eq!(repos[0]["branches"][0]["name"], "main");

    let audit_path = home_tmp.path().join(".gnx/audit.log");
    assert!(audit_path.exists(), "audit.log not written");
    let audit_content = std::fs::read_to_string(&audit_path).unwrap();
    assert!(audit_content.contains("\"event\":\"analyze.complete\""));
}
