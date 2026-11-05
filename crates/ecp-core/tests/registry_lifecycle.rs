//! End-to-end registry lifecycle: open → upsert → reopen.

use ecp_core::registry::{Registry, RepoAlias};

#[test]
fn lifecycle_create_upsert_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path();

    let mut reg = Registry::open(home_ecp).unwrap();
    assert_eq!(reg.snapshot().repos.len(), 0);

    reg.upsert_repo(RepoAlias {
        dir_name: "foo__abcd1234".into(),
        common_dir: home_ecp.join("foo/.git").to_string_lossy().into(),
        remote_url: Some("git@github.com:x/foo.git".into()),
        aliases: vec!["foo".into()],
        last_touched: "2026-05-14T00:00:00Z".into(),
        groups: vec![],
    })
    .unwrap();

    drop(reg);
    let reg2 = Registry::open(home_ecp).unwrap();
    let snap = reg2.snapshot();
    assert_eq!(snap.repos.len(), 1);
    assert!(snap.repos.contains_key("foo__abcd1234"));
    assert_eq!(snap.repos["foo__abcd1234"].aliases, vec!["foo".to_string()]);
}

#[cfg(unix)]
#[test]
fn test_registry_open_on_private_dir_issues_no_chmod() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path().join("ecp");
    std::fs::create_dir(&home_ecp).unwrap();
    std::fs::set_permissions(&home_ecp, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ctime = |p: &std::path::Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.ctime(), m.ctime_nsec())
    };
    let before = ctime(&home_ecp);
    // A chmod updates ctime even when the mode does not change; a read-only
    // filesystem refuses that chmod with EROFS.
    std::thread::sleep(std::time::Duration::from_millis(20));
    Registry::open(&home_ecp).unwrap();
    assert_eq!(ctime(&home_ecp), before);
}

#[cfg(unix)]
#[test]
fn test_registry_open_on_open_dir_tightens_it_to_0700() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home_ecp = tmp.path().join("ecp");
    std::fs::create_dir(&home_ecp).unwrap();
    std::fs::set_permissions(&home_ecp, std::fs::Permissions::from_mode(0o755)).unwrap();
    Registry::open(&home_ecp).unwrap();
    let mode = std::fs::metadata(&home_ecp).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

/// Fails on a `read_or_empty` that tests `path.exists()` first: `exists()`
/// is false when the directory refuses the stat (another owner's 0700
/// `~/.ecp`), so the registry would read as empty and the next write would
/// drop every repo.
#[cfg(unix)]
#[test]
fn test_read_or_empty_in_unsearchable_dir_returns_permission_denied() {
    use ecp_core::registry::RegistryFile;
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("ecp");
    std::fs::create_dir(&locked).unwrap();
    let registry = locked.join("registry.json");
    std::fs::write(&registry, "{}").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = RegistryFile::read_or_empty(&registry);
    let searchable = std::fs::metadata(&registry).is_ok();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    if searchable {
        return; // running as root: mode 000 does not block the stat
    }
    assert_eq!(
        result.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
}
