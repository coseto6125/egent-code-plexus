//! Tests for registry.json schema (spec §2).

use graph_nexus_core::registry::{
    strip_credentials, BranchEntry, BranchMeta, RegistryFile, RepoEntry,
};

#[test]
fn round_trip_serialize_deserialize() {
    let original = RegistryFile {
        version: 1,
        repos: vec![RepoEntry {
            name: "graph-nexus".into(),
            remote_url: "git@github.com:coseto6125/graph-nexus.git".into(),
            worktree_path: "/home/enor/graph-nexus".into(),
            index_dir_root: "/home/enor/.gnx/graph-nexus".into(),
            branches: vec![BranchEntry {
                name: "main".into(),
                index_dir: "/home/enor/.gnx/graph-nexus/main".into(),
                indexed_at: "2026-05-14T03:00:00Z".into(),
                node_count: 12453,
                delta_size: 0,
                embedding_status: "complete".into(),
            }],
            group: None,
        }],
        groups: vec![],
    };

    let json = serde_json::to_string_pretty(&original).unwrap();
    let parsed: RegistryFile = serde_json::from_str(&json).unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn parses_v1_with_missing_groups_field() {
    let json = r#"{"version":1,"repos":[]}"#;
    let parsed: RegistryFile = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.version, 1);
    assert!(parsed.repos.is_empty());
    assert!(parsed.groups.is_empty());
}

#[test]
fn atomic_write_creates_bak_on_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("registry.json");
    let bak = tmp.path().join("registry.json.bak");

    // First write — no .bak yet
    let v1 = RegistryFile {
        version: 1,
        repos: vec![],
        groups: vec![],
    };
    RegistryFile::write_atomic(&path, &v1).unwrap();
    assert!(path.exists());
    assert!(!bak.exists());

    // Second write — should backup v1 into .bak
    let v2 = RegistryFile {
        version: 1,
        repos: vec![RepoEntry {
            name: "foo".into(),
            remote_url: "".into(),
            worktree_path: "".into(),
            index_dir_root: "".into(),
            branches: vec![],
            group: None,
        }],
        groups: vec![],
    };
    RegistryFile::write_atomic(&path, &v2).unwrap();
    assert!(bak.exists());

    let bak_content: RegistryFile =
        serde_json::from_str(&std::fs::read_to_string(&bak).unwrap()).unwrap();
    assert_eq!(bak_content, v1);

    let current: RegistryFile =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(current, v2);
}

#[test]
fn read_returns_empty_when_file_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("registry.json");
    let r = RegistryFile::read_or_empty(&path).unwrap();
    assert_eq!(r, RegistryFile::empty());
}

#[test]
fn read_falls_back_to_bak_on_corrupt_main() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("registry.json");
    let bak = tmp.path().join("registry.json.bak");

    let v1 = RegistryFile {
        version: 1,
        repos: vec![],
        groups: vec![],
    };
    RegistryFile::write_atomic(&path, &v1).unwrap();
    let v2 = v1.clone();
    RegistryFile::write_atomic(&path, &v2).unwrap();
    assert!(bak.exists());

    std::fs::write(&path, "garbage{").unwrap();

    let r = RegistryFile::read_or_empty(&path).unwrap();
    assert_eq!(r, v1);
}

#[test]
fn strips_user_pass_from_https() {
    let s = strip_credentials("https://user:TOKEN@github.com/foo/bar.git");
    assert_eq!(s, "https://github.com/foo/bar.git");
}

#[test]
fn leaves_clean_https_untouched() {
    let s = strip_credentials("https://github.com/foo/bar.git");
    assert_eq!(s, "https://github.com/foo/bar.git");
}

#[test]
fn leaves_ssh_url_untouched() {
    let s = strip_credentials("git@github.com:foo/bar.git");
    assert_eq!(s, "git@github.com:foo/bar.git");
}

#[test]
fn rebuild_from_disk_when_both_registry_and_bak_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let home_gnx = tmp.path();

    // Manually create ~/.gnx/foo/main/meta.json
    let meta_dir = home_gnx.join("foo").join("main");
    std::fs::create_dir_all(&meta_dir).unwrap();
    let meta = BranchMeta {
        indexed_at: "2026-05-14T03:00:00Z".into(),
        node_count: 50,
        delta_size: 0,
        last_compact_at: None,
        worktree_path: "/some/path".into(),
        remote_url: "https://example.com/foo.git".into(),
        schema_version: 1,
    };
    BranchMeta::write_atomic(&meta_dir.join("meta.json"), &meta).unwrap();

    let rebuilt = RegistryFile::rebuild_from_disk(home_gnx).unwrap();
    assert_eq!(rebuilt.repos.len(), 1);
    assert_eq!(rebuilt.repos[0].name, "foo");
    assert_eq!(rebuilt.repos[0].worktree_path, "/some/path");
    assert_eq!(rebuilt.repos[0].remote_url, "https://example.com/foo.git");
    assert_eq!(rebuilt.repos[0].branches.len(), 1);
    assert_eq!(rebuilt.repos[0].branches[0].name, "main");
    assert_eq!(rebuilt.repos[0].branches[0].node_count, 50);
}
