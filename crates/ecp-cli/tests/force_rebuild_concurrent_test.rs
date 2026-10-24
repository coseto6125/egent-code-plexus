use std::process::Command;
use std::thread;

fn ecp_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("ecp")
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
fn two_concurrent_force_rebuilds_both_succeed_with_one_new_generation() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let _sha = git_init(wt.path());
    let home_path = home.path().to_path_buf();
    let wt_path = wt.path().to_path_buf();

    // Seed an initial L2 so we exercise the drop-existing path
    Command::new(ecp_bin())
        .env("HOME", &home_path)
        .args(["admin", "index", "--repo"])
        .arg(&wt_path)
        .status()
        .unwrap();

    let h1 = {
        let home_path = home_path.clone();
        let wt_path = wt_path.clone();
        thread::spawn(move || {
            Command::new(ecp_bin())
                .env("HOME", &home_path)
                .args(["admin", "index", "--repo"])
                .arg(&wt_path)
                .arg("--force")
                .output()
                .unwrap()
        })
    };
    let h2 = {
        let home_path = home_path.clone();
        let wt_path = wt_path.clone();
        thread::spawn(move || {
            Command::new(ecp_bin())
                .env("HOME", &home_path)
                .args(["admin", "index", "--repo"])
                .arg(&wt_path)
                .arg("--force")
                .output()
                .unwrap()
        })
    };
    let o1 = h1.join().unwrap();
    let o2 = h2.join().unwrap();

    assert!(
        o1.status.success(),
        "process 1 failed: {}",
        String::from_utf8_lossy(&o1.stderr)
    );
    assert!(
        o2.status.success(),
        "process 2 failed: {}",
        String::from_utf8_lossy(&o2.stderr)
    );

    // One rebuilt generation for this SHA. The two forces hold the SHA lock
    // one after the other and each builds, a waiter too: what the first
    // published may be any build, and only a build of its own drops the parse
    // cache. The second publishes above the first, and its sweep drops every
    // lower generation. One generation and no staging remain.
    let ecp_home = home_path.join(".ecp");
    let (commit_dirs, building_dirs) = count_dirs(&ecp_home);
    assert_eq!(
        commit_dirs, 1,
        "expected only the rebuilt generation, found {commit_dirs}"
    );
    assert_eq!(
        building_dirs, 0,
        "expected no staging leftovers, found {building_dirs}"
    );
}

/// Recursively count: (a) published commit dirs (`branch_*`, not a recovery
/// name), (b) staging dirs of either shape (`.building`,
/// `.building.<pid>.<n>`), retired or not, at any depth.
fn count_dirs(root: &std::path::Path) -> (usize, usize) {
    let mut commits = 0;
    let mut building = 0;
    fn walk(p: &std::path::Path, c: &mut usize, b: &mut usize) {
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(_) => return,
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let recovery = ecp_cli::commit_lookup::is_recovery_dir_name(&name);
            if recovery && name.contains(".building") {
                *b += 1;
            }
            if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                if !name.starts_with("branch_") {
                    walk(&e.path(), c, b);
                } else if !recovery {
                    *c += 1;
                }
            }
        }
    }
    walk(root, &mut commits, &mut building);
    (commits, building)
}
