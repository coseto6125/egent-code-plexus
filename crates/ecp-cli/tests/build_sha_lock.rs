//! The SHA lock seen from the public entry points: one name and one lock per
//! SHA however it is spelled, one publish however many builders race, and a
//! clean failure that leaves nothing behind. Tests that set `ECP_HOME` or
//! `PATH` serialize on `ENV`.

use ecp_cli::build::orchestrator::build_l2;
use ecp_cli::commit_lookup::CommitDirListing;
use ecp_core::registry::FileLock;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, Mutex, MutexGuard};

static ENV: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV.lock().unwrap_or_else(|e| e.into_inner())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

struct World {
    _tmp: tempfile::TempDir,
    wt: PathBuf,
    home: PathBuf,
    sha: String,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q"]);
        std::fs::write(wt.join("a.py"), "def a():\n    return 1\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "one"]);
        let sha = git(&wt, &["rev-parse", "HEAD"]);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _tmp: tmp,
            wt,
            home,
            sha,
        }
    }

    /// Points this process's ecp home at the world. Hold `env_lock`.
    fn activate(&self) {
        std::env::set_var("ECP_HOME", &self.home);
    }

    fn commits(&self) -> PathBuf {
        self.home
            .join(ecp_cli::repo_identity::repo_dir_name_for_cwd(&self.wt).unwrap())
            .join("commits")
    }

    fn lock_files(&self) -> Vec<String> {
        std::fs::read_dir(self.commits().join(ecp_cli::commit_lookup::LOCKS_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".lock"))
            .collect()
    }
}

/// Every path a build derives from the SHA is the lowercase hex, so `ABC…`
/// and `abc…` share one lock and one dir.
#[test]
fn test_build_l2_uppercase_sha_lowercase_name_and_lock() {
    let _env = env_lock();
    let w = World::new();
    w.activate();

    let mut upper = build_l2(&w.wt, Some(&w.sha.to_uppercase())).unwrap();
    upper.join_background();
    let name = upper.commit_dir.file_name().unwrap().to_string_lossy();
    assert!(name.ends_with(&w.sha), "{name}");
    assert_eq!(w.lock_files(), vec![format!("{}.lock", &w.sha[..2])]);

    let mut lower = build_l2(&w.wt, Some(&w.sha)).unwrap();
    lower.join_background();
    assert_eq!(lower.commit_dir, upper.commit_dir);
    assert_eq!(CommitDirListing::read(&w.commits()).unwrap().dirs.len(), 1);
}

/// Two builders of one SHA started together: the second waits on the SHA
/// lock and attaches to what the first published.
#[test]
fn test_build_l2_concurrent_same_sha_one_publish() {
    let _env = env_lock();
    let w = World::new();
    w.activate();
    let barrier = Barrier::new(2);

    let dirs: Vec<PathBuf> = std::thread::scope(|s| {
        let builders: Vec<_> = (0..2)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    let mut r = build_l2(&w.wt, None).unwrap();
                    r.join_background();
                    r.commit_dir
                })
            })
            .collect();
        builders.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert_eq!(dirs[0], dirs[1]);
    let listing = CommitDirListing::read(&w.commits()).unwrap();
    assert_eq!(listing.dirs.len(), 1);
    assert!(listing.staging.is_empty());
}

/// git missing: the build fails with an error and leaves no dir and no
/// staging. It fails before it takes the SHA lock, so the release of a held
/// lock on failure is covered by
/// `orchestrator::tests::test_build_inside_locked_sync_file_fails_lock_free_no_staging`.
#[test]
fn test_build_l2_git_missing_error_nothing_left() {
    let _env = env_lock();
    let w = World::new();
    w.activate();
    let commits = w.commits();
    let path = std::env::var_os("PATH");
    std::env::set_var("PATH", "");
    let result = build_l2(&w.wt, Some(&w.sha));
    match path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }

    assert!(result.is_err());
    let listing = CommitDirListing::read(&commits).unwrap();
    assert!(
        listing.dirs.is_empty() && listing.staging.is_empty(),
        "{listing:?}"
    );
    let mut sha = [0u8; 20];
    hex::decode_to_slice(&w.sha, &mut sha).unwrap();
    let lock = ecp_cli::commit_lookup::sha_lock_path(&commits, &sha);
    assert!(
        FileLock::try_existing(&lock).is_ok(),
        "the SHA lock is not held"
    );
}

fn ecp_index(w: &World) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ecp"))
        .env("ECP_HOME", &w.home)
        .env("HOME", &w.home)
        .env("ECP_SKIP_BG_REBUILD", "1")
        .args(["admin", "index", "--repo"])
        .arg(&w.wt)
        .output()
        .unwrap()
}

/// F11: a graph another ecp version built is no reusable build. `admin index`
/// used to answer `l2.exists` for it while every query rejected it on its
/// fingerprint; it now rebuilds and drops the superseded dir.
#[test]
fn test_admin_index_stale_fingerprint_rebuilds() {
    // The child inherits this process's `PATH`, which the git-missing test
    // empties for a moment.
    let _env = env_lock();
    let w = World::new();
    let first = ecp_index(&w);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let commits = w.commits();
    let listing = CommitDirListing::read(&commits).unwrap();
    assert_eq!(listing.dirs.len(), 1);
    let old = commits.join(&listing.dirs[0].name);
    let meta_path = old.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["builder_fingerprint"] = "v0.0.1+schema1".into();
    std::fs::write(&meta_path, meta.to_string()).unwrap();
    std::fs::write(
        ecp_cli::auto_ensure::builder_fingerprint_sidecar_path(&old.join("graph.bin")),
        "v0.0.1+schema1\n",
    )
    .unwrap();

    let second = ecp_index(&w);

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(second.status.success(), "{stderr}");
    assert!(stderr.contains("l2.built"), "{stderr}");
    assert!(!stderr.contains("l2.exists"), "{stderr}");
    let listing = CommitDirListing::read(&commits).unwrap();
    assert_eq!(listing.dirs.len(), 1, "{listing:?}");
    assert!(!old.exists(), "the stale build is superseded and dropped");
}
