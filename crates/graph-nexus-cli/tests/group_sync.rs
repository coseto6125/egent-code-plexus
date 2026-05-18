//! Integration test for `gnx group sync`.
//!
//! Sets up two minimal git repos — one Go file (HTTP provider) and one
//! Python file (HTTP consumer on the same path), indexes both, forms a
//! group, syncs it, then asserts contracts.rkyv + meta.json are written
//! and non-empty.

use graph_nexus_cli::commands::group::storage::{group_dir, read_contracts};
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn git_init_and_commit(dir: &Path) {
    Command::new("git")
        .current_dir(dir)
        .args(["init", "-q"])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(dir)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(dir)
        .args([
            "-c", "user.email=t@t",
            "-c", "user.name=t",
            "commit", "-qm", "init",
        ])
        .status()
        .unwrap();
}

fn run_gnx(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(gnx_bin())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("gnx spawn failed")
}

#[test]
fn group_sync_writes_contracts_and_meta() {
    // ── 1. Create two temp repos ──────────────────────────────────────────
    let repos_tmp = tempfile::tempdir().unwrap();

    // Go repo: HTTP provider on /api/users
    let go_repo = repos_tmp.path().join("svc-go");
    std::fs::create_dir_all(&go_repo).unwrap();
    std::fs::write(
        go_repo.join("main.go"),
        r#"package main
import "net/http"
func main() {
    mux := http.NewServeMux()
    mux.HandleFunc("/api/users", createUser)
}
func createUser(w http.ResponseWriter, r *http.Request) {}
"#,
    )
    .unwrap();
    git_init_and_commit(&go_repo);

    // Python repo: HTTP consumer on /api/users
    let py_repo = repos_tmp.path().join("svc-py");
    std::fs::create_dir_all(&py_repo).unwrap();
    std::fs::write(
        py_repo.join("app.py"),
        r#"from flask import Flask
app = Flask(__name__)

@app.route("/api/users", methods=["POST"])
def create_user():
    return ""
"#,
    )
    .unwrap();
    git_init_and_commit(&py_repo);

    // ── 2. Isolated GNX_HOME ──────────────────────────────────────────────
    let home_tmp = tempfile::tempdir().unwrap();
    let home = home_tmp.path();

    // ── 3. Index both repos ───────────────────────────────────────────────
    let out = run_gnx(&["admin", "index", "--repo", go_repo.to_str().unwrap()], home);
    assert!(
        out.status.success(),
        "admin index go failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let out = run_gnx(&["admin", "index", "--repo", py_repo.to_str().unwrap()], home);
    assert!(
        out.status.success(),
        "admin index py failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // ── 4. Discover the dir_name aliases assigned by the registry ─────────
    //    `admin index` registers repos by the dir_name computed from git common-dir.
    //    We read the registry to find actual dir_name values so we can `group add`
    //    using names that actually exist.
    let registry_path = home.join(".gnx").join("registry.json");
    let reg = graph_nexus_core::registry::RegistryFile::read_or_empty(&registry_path).unwrap();

    let go_dir_name = reg
        .repos
        .keys()
        .find(|k| k.starts_with("svc-go"))
        .cloned()
        .expect("svc-go repo not in registry");
    let py_dir_name = reg
        .repos
        .keys()
        .find(|k| k.starts_with("svc-py"))
        .cloned()
        .expect("svc-py repo not in registry");

    // ── 5. Add both repos to the "demo" group ────────────────────────────
    let out = run_gnx(&["admin", "group", "add", &go_dir_name, "demo"], home);
    assert!(
        out.status.success(),
        "admin group add go failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let out = run_gnx(&["admin", "group", "add", &py_dir_name, "demo"], home);
    assert!(
        out.status.success(),
        "admin group add py failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    // ── 6. Run group sync ─────────────────────────────────────────────────
    let out = run_gnx(&["group", "sync", "demo"], home);
    assert!(
        out.status.success(),
        "group sync failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // ── 7. Verify contracts.rkyv and meta.json exist ──────────────────────
    let gnx_home = home.join(".gnx");
    let gdir = group_dir(&gnx_home, "demo");
    assert!(
        gdir.join("contracts.rkyv").exists(),
        "contracts.rkyv missing at {:?}",
        gdir
    );
    assert!(
        gdir.join("meta.json").exists(),
        "meta.json missing at {:?}",
        gdir
    );

    // ── 8. Verify contract content ────────────────────────────────────────
    let contract_reg = read_contracts(&gdir).unwrap();
    assert!(
        contract_reg.contracts.len() >= 2,
        "expected at least 2 contracts, got {}",
        contract_reg.contracts.len()
    );

    // ── 9. Verify both repo names appear in meta.repo_snapshots ──────────
    let meta_bytes = std::fs::read(gdir.join("meta.json")).unwrap();
    let meta: graph_nexus_cli::commands::group::storage::GroupMeta =
        serde_json::from_slice(&meta_bytes).unwrap();

    assert!(
        meta.repo_snapshots.contains_key(&go_dir_name),
        "go repo snapshot missing; got keys: {:?}",
        meta.repo_snapshots.keys().collect::<Vec<_>>()
    );
    assert!(
        meta.repo_snapshots.contains_key(&py_dir_name),
        "py repo snapshot missing; got keys: {:?}",
        meta.repo_snapshots.keys().collect::<Vec<_>>()
    );
}
