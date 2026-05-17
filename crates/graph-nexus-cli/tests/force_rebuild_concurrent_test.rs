use std::process::Command;
use std::thread;

fn gnx_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("gnx")
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
fn two_concurrent_force_rebuilds_both_succeed_with_one_final_commit_dir() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    let _sha = git_init(wt.path());
    let home_path = home.path().to_path_buf();
    let wt_path = wt.path().to_path_buf();

    // Seed an initial L2 so we exercise the drop-existing path
    Command::new(gnx_bin())
        .env("HOME", &home_path)
        .args(["admin", "index", "--repo"])
        .arg(&wt_path)
        .status()
        .unwrap();

    let h1 = {
        let home_path = home_path.clone();
        let wt_path = wt_path.clone();
        thread::spawn(move || {
            Command::new(gnx_bin())
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
            Command::new(gnx_bin())
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

    // Only one commit_dir for this SHA + no leftover .building
    let gnx_home = home_path.join(".gnx");
    let (commit_dirs, building_dirs) = count_dirs(&gnx_home);
    assert_eq!(
        commit_dirs, 1,
        "expected exactly 1 commit dir, found {commit_dirs}"
    );
    assert_eq!(
        building_dirs, 0,
        "expected no .building leftovers, found {building_dirs}"
    );
}

/// Recursively count: (a) commit dirs whose name starts with "branch_",
/// (b) `.building` dirs at any depth.
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
            if name.ends_with(".building") {
                *b += 1;
            }
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    if name.starts_with("branch_") {
                        *c += 1;
                    } else {
                        walk(&e.path(), c, b);
                    }
                }
            }
        }
    }
    walk(root, &mut commits, &mut building);
    (commits, building)
}
