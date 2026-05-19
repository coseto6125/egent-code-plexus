use cgn_core::registry::{RegistryFile, RepoAlias};
use std::collections::BTreeMap;
use tempfile::NamedTempFile;

#[test]
fn v2_empty_registry() {
    let reg = RegistryFile::empty();
    assert_eq!(reg.version, 2);
    assert!(reg.repos.is_empty());
    assert!(reg.groups.is_empty());
}

#[test]
fn v2_round_trip_deterministic_and_full_equality() {
    let mut repos = BTreeMap::new();
    repos.insert(
        "a__1234".into(),
        RepoAlias {
            dir_name: "a__1234".into(),
            common_dir: "/work/a/.git".into(),
            remote_url: None,
            aliases: vec!["a".into()],
            last_touched: "2026-05-17T10:00:00Z".into(),
            groups: vec![],
        },
    );
    repos.insert(
        "b__5678".into(),
        RepoAlias {
            dir_name: "b__5678".into(),
            common_dir: "/work/b/.git".into(),
            remote_url: Some("https://github.com/u/b.git".into()),
            aliases: vec!["b".into(), "beta".into()],
            last_touched: "2026-05-17T11:00:00Z".into(),
            groups: vec!["backend".into()],
        },
    );
    let reg = RegistryFile {
        version: 2,
        repos,
        groups: vec![],
    };
    let s1 = serde_json::to_string(&reg).unwrap();
    let s2 = serde_json::to_string(&reg).unwrap();
    assert_eq!(s1, s2);
    // BTreeMap → sorted keys: a__1234 before b__5678
    assert!(s1.find("a__1234").unwrap() < s1.find("b__5678").unwrap());
    let back: RegistryFile = serde_json::from_str(&s1).unwrap();
    assert_eq!(back, reg);
}

#[test]
fn v1_auto_migrates_via_rebuild_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let registry_path = home.join("registry.json");
    std::fs::write(&registry_path, r#"{"version":1,"repos":[]}"#).unwrap();

    let repo_dir = home.join("myrepo__abcd1234");
    std::fs::create_dir(&repo_dir).unwrap();
    let repo_meta = cgn_core::registry::RepoMeta {
        version: 1,
        common_dir: "/work/myrepo/.git".into(),
        remote_url: Some("https://github.com/u/r.git".into()),
        aliases: vec!["myrepo".into()],
        known_refs: BTreeMap::new(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    cgn_core::registry::RepoMeta::write_atomic(&repo_dir.join("meta.json"), &repo_meta)
        .unwrap();

    let reg = RegistryFile::read_or_empty(&registry_path).unwrap();
    assert_eq!(reg.version, 2, "auto-migrated registry must be v2");
    assert_eq!(
        reg.repos.len(),
        1,
        "rebuild_from_disk must populate from meta.json"
    );
    assert!(reg.repos.contains_key("myrepo__abcd1234"));

    // Persisted to disk as v2 — next call must not re-migrate.
    #[derive(serde::Deserialize)]
    struct Probe {
        version: u32,
    }
    let disk_bytes = std::fs::read(&registry_path).unwrap();
    let probe: Probe = serde_json::from_slice(&disk_bytes).unwrap();
    assert_eq!(probe.version, 2, "migration must persist v2 to disk");
}

#[test]
fn v1_auto_migrates_with_empty_home_yields_empty_v2() {
    let tmp = tempfile::tempdir().unwrap();
    let registry_path = tmp.path().join("registry.json");
    std::fs::write(&registry_path, r#"{"version":1,"repos":[]}"#).unwrap();

    let reg = RegistryFile::read_or_empty(&registry_path).unwrap();
    assert_eq!(reg.version, 2);
    assert!(reg.repos.is_empty());
}

#[test]
fn missing_repos_field_defaults_to_empty() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), r#"{"version":2}"#).unwrap();
    let reg = RegistryFile::read_or_empty(tmp.path()).unwrap();
    assert!(reg.repos.is_empty());
    assert!(reg.groups.is_empty());
}

#[test]
fn write_atomic_does_not_create_bak() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path();
    std::fs::write(path, r#"{"version":2,"repos":{},"groups":[]}"#).unwrap();
    let mut bak_buf = path.as_os_str().to_owned();
    bak_buf.push(".bak");
    let bak = std::path::PathBuf::from(bak_buf);
    let _ = std::fs::remove_file(&bak); // ensure clean

    let reg = RegistryFile::empty();
    RegistryFile::write_atomic(path, &reg).unwrap();
    assert!(
        !bak.exists(),
        ".bak must NOT be created — spec §3 layout has no .bak; recovery goes through rebuild_from_disk"
    );
}

#[test]
fn rebuild_from_disk_walks_repo_meta_json() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let repo_dir = home.join("myrepo__abcd1234");
    std::fs::create_dir(&repo_dir).unwrap();
    let repo_meta = cgn_core::registry::RepoMeta {
        version: 1,
        common_dir: "/work/myrepo/.git".into(),
        remote_url: Some("https://github.com/u/r.git".into()),
        aliases: vec!["myrepo".into()],
        known_refs: BTreeMap::new(),
        last_built_sha: None,
        total_size_bytes: 0,
        last_touched: "2026-05-17T10:00:00Z".into(),
    };
    cgn_core::registry::RepoMeta::write_atomic(&repo_dir.join("meta.json"), &repo_meta)
        .unwrap();

    let reg = RegistryFile::rebuild_from_disk(home).unwrap();
    assert_eq!(reg.repos.len(), 1);
    let alias = reg.repos.get("myrepo__abcd1234").unwrap();
    assert_eq!(alias.common_dir, "/work/myrepo/.git");
    assert_eq!(alias.aliases, vec!["myrepo".to_string()]);
}

#[test]
fn rebuild_from_disk_skips_hidden_and_underscore_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir(home.join("_sessions")).unwrap();
    std::fs::create_dir(home.join(".cache")).unwrap();
    let reg = RegistryFile::rebuild_from_disk(home).unwrap();
    assert!(reg.repos.is_empty());
}
