//! End-to-end registry lifecycle: open → upsert → reopen.

use cgn_core::registry::{Registry, RepoAlias};

#[test]
fn lifecycle_create_upsert_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let home_gnx = tmp.path();

    let mut reg = Registry::open(home_gnx).unwrap();
    assert_eq!(reg.snapshot().repos.len(), 0);

    reg.upsert_repo(RepoAlias {
        dir_name: "foo__abcd1234".into(),
        common_dir: home_gnx.join("foo/.git").to_string_lossy().into(),
        remote_url: Some("git@github.com:x/foo.git".into()),
        aliases: vec!["foo".into()],
        last_touched: "2026-05-14T00:00:00Z".into(),
        groups: vec![],
    })
    .unwrap();

    drop(reg);
    let reg2 = Registry::open(home_gnx).unwrap();
    let snap = reg2.snapshot();
    assert_eq!(snap.repos.len(), 1);
    assert!(snap.repos.contains_key("foo__abcd1234"));
    assert_eq!(snap.repos["foo__abcd1234"].aliases, vec!["foo".to_string()]);
}
