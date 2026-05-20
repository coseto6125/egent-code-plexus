use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

/// `ecp admin prune --branch` is a no-op in v2 (branch isn't stored).
/// The command now returns an error directing users to --orphans.
#[test]
fn prune_branch_returns_informative_error() {
    let home_tmp = tempfile::tempdir().unwrap();
    let repo_tmp = tempfile::tempdir().unwrap();

    // Init git repo
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo_tmp.path())
        .output()
        .unwrap();

    let out = Command::new(ecp_bin())
        .args([
            "admin",
            "prune",
            "--branch",
            "feat-x",
            "--repo",
            &repo_tmp.path().display().to_string(),
        ])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    // v2: --branch prune is stubbed and returns an error
    assert!(
        !out.status.success(),
        "expected error for --branch in v2; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-op") || stderr.contains("v2") || stderr.contains("orphans"),
        "expected informative v2 message; stderr={stderr}"
    );
}

#[test]
fn prune_orphans_drops_entries_with_missing_common_dir() {
    use ecp_core::registry::{RegistryFile, RepoAlias};
    use std::collections::BTreeMap;

    let home_tmp = tempfile::tempdir().unwrap();
    let home_ecp = home_tmp.path().join(".ecp");
    std::fs::create_dir_all(&home_ecp).unwrap();

    // valid-repo: common_dir exists (use the home_ecp dir itself as a stand-in)
    let valid_common_dir = home_ecp.clone();
    let valid_index = home_ecp.join("valid-repo__aabbccdd");
    // orphan-repo: common_dir does NOT exist
    let orphan_index = home_ecp.join("orphan-repo__aabbccdd");
    std::fs::create_dir_all(valid_index.join("commits")).unwrap();
    std::fs::create_dir_all(orphan_index.join("commits").join("sha_abc12345")).unwrap();
    std::fs::write(orphan_index.join("commits/sha_abc12345/graph.bin"), b"junk").unwrap();

    let mut repos = BTreeMap::new();
    repos.insert(
        "valid-repo__aabbccdd".into(),
        RepoAlias {
            dir_name: "valid-repo__aabbccdd".into(),
            common_dir: valid_common_dir.to_string_lossy().into_owned(),
            remote_url: Some("git@github.com:E-NoR/valid.git".into()),
            aliases: vec!["valid-repo".into()],
            last_touched: "2026-05-16T00:00:00Z".into(),
            groups: vec![],
        },
    );
    repos.insert(
        "orphan-repo__aabbccdd".into(),
        RepoAlias {
            dir_name: "orphan-repo__aabbccdd".into(),
            common_dir: "/nonexistent/path/that/does/not/exist/.git".into(),
            remote_url: Some("git@github.com:E-NoR/orphan.git".into()),
            aliases: vec!["orphan-repo".into()],
            last_touched: "2026-05-16T00:00:00Z".into(),
            groups: vec![],
        },
    );
    let registry = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };
    let registry_path = home_ecp.join("registry.json");
    std::fs::write(&registry_path, serde_json::to_string(&registry).unwrap()).unwrap();

    let out = Command::new(ecp_bin())
        .args(["admin", "prune", "--orphans"])
        .env("HOME", home_tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "admin prune --orphans failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !orphan_index.exists(),
        "orphan repo's index dir should be removed"
    );
    assert!(valid_index.exists(), "valid repo's index dir should remain");

    let updated: RegistryFile =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).unwrap()).unwrap();
    assert_eq!(
        updated.repos.len(),
        1,
        "expected one repo to remain after orphan sweep"
    );
    assert!(
        updated.repos.contains_key("valid-repo__aabbccdd"),
        "valid-repo should remain"
    );
}
