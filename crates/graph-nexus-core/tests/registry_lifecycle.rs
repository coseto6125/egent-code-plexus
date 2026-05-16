//! End-to-end registry lifecycle: open → upsert → reopen.

use graph_nexus_core::registry::{BranchEntry, Registry, RepoEntry};

#[test]
fn lifecycle_create_upsert_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let home_gnx = tmp.path();

    let mut reg = Registry::open(home_gnx).unwrap();
    assert_eq!(reg.snapshot().repos.len(), 0);

    reg.upsert_repo(RepoEntry {
        name: "foo".into(),
        remote_url: "git@github.com:x/foo.git".into(),
        worktree_path: "/path/foo".into(),
        index_dir_root: home_gnx.join("foo").to_string_lossy().into(),
        branches: vec![BranchEntry {
            name: "main".into(),
            index_dir: home_gnx.join("foo/main").to_string_lossy().into(),
            indexed_at: "2026-05-14T00:00:00Z".into(),
            node_count: 100,
            delta_size: 0,
        }],
        groups: vec![],
    })
    .unwrap();

    drop(reg);
    let reg2 = Registry::open(home_gnx).unwrap();
    let snap = reg2.snapshot();
    assert_eq!(snap.repos.len(), 1);
    assert_eq!(snap.repos[0].name, "foo");
    assert_eq!(snap.repos[0].branches[0].node_count, 100);
}
