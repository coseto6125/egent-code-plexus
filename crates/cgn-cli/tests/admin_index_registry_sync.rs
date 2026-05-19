//! Regression: `admin index` / `build_l2` must register the repo in
//! `~/.gnx/registry.json` so cross-repo commands (`contracts`, `coverage`,
//! `--repo @all`) can see freshly indexed repos.
//!
//! Pre-fix: `build_inside_locked` wrote per-commit `meta.json` and per-repo
//! `meta.json` but never touched the global registry. `contracts --repo @all`
//! was blind to anything indexed since the registry was last (manually)
//! rebuilt — `rebuild_from_disk` was defined but had zero callers.

use cgn_cli::build::orchestrator;
use cgn_cli::repo_identity::repo_dir_name_for_cwd;
use cgn_core::registry::RegistryFile;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// `build_l2` resolves the L2 root via `HOME`. Cargo runs integration tests
/// in parallel threads within the same binary — every test that mutates
/// `HOME` serialises through this guard.
static HOME_GUARD: Mutex<()> = Mutex::new(());

fn lock_home() -> std::sync::MutexGuard<'static, ()> {
    HOME_GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

fn init_repo_with_commit(worktree: &Path) {
    Command::new("git")
        .current_dir(worktree)
        .args(["init", "-q"])
        .status()
        .unwrap();
    std::fs::write(worktree.join("main.rs"), "fn main() {}\n").unwrap();
    Command::new("git")
        .current_dir(worktree)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(worktree)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ])
        .status()
        .unwrap();
}

#[test]
fn build_l2_registers_repo_in_global_registry() {
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    let home = tmp.path().join("home");
    std::env::set_var("HOME", &home);

    let dir_name = repo_dir_name_for_cwd(&worktree).unwrap();
    let _ = orchestrator::build_l2(&worktree, None).unwrap();

    let registry_path = home.join(".gnx").join("registry.json");
    assert!(
        registry_path.exists(),
        "registry.json must exist after build_l2"
    );
    let reg = RegistryFile::read_or_empty(&registry_path).unwrap();
    assert!(
        reg.repos.contains_key(&dir_name),
        "registry.repos must contain {dir_name}; got keys: {:?}",
        reg.repos.keys().collect::<Vec<_>>()
    );
    let entry = &reg.repos[&dir_name];
    assert_eq!(entry.dir_name, dir_name);
    assert!(
        !entry.common_dir.is_empty(),
        "common_dir must be populated from RepoMeta"
    );
}

#[test]
fn rebuild_preserves_user_group_membership() {
    let _g = lock_home();
    let tmp = tempfile::tempdir().unwrap();
    let worktree = tmp.path().join("wt");
    std::fs::create_dir(&worktree).unwrap();
    init_repo_with_commit(&worktree);

    let home = tmp.path().join("home");
    std::env::set_var("HOME", &home);

    let dir_name = repo_dir_name_for_cwd(&worktree).unwrap();

    // 1. First build → registry entry created (groups: [])
    let _ = orchestrator::build_l2(&worktree, None).unwrap();

    // 2. Simulate `gnx admin group add <repo> squad`
    let registry_path = home.join(".gnx").join("registry.json");
    let mut reg = RegistryFile::read_or_empty(&registry_path).unwrap();
    reg.repos
        .get_mut(&dir_name)
        .expect("repo registered by first build_l2")
        .groups
        .push("squad".to_string());
    RegistryFile::write_atomic(&registry_path, &reg).unwrap();

    // 3. Advance HEAD → new SHA → build_l2 takes the rebuild path that
    //    re-enters update_repo_meta, which now re-upserts the registry.
    std::fs::write(worktree.join("main.rs"), "fn main() { let _ = 2; }\n").unwrap();
    Command::new("git")
        .current_dir(&worktree)
        .args(["add", "."])
        .status()
        .unwrap();
    Command::new("git")
        .current_dir(&worktree)
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "v2",
        ])
        .status()
        .unwrap();
    let _ = orchestrator::build_l2(&worktree, None).unwrap();

    let reg2 = RegistryFile::read_or_empty(&registry_path).unwrap();
    assert_eq!(
        reg2.repos
            .get(&dir_name)
            .expect("repo still registered")
            .groups,
        vec!["squad".to_string()],
        "user-added group membership must survive a re-index"
    );
}
