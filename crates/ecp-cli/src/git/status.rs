//! `git status --porcelain -z --untracked-files=all`, the dirty-set query the
//! freshness gate runs on every graph query.
//!
//! Most of its cost is the untracked-file walk. git can skip unchanged
//! directories through its untracked cache, but that cache lives inside the
//! index, and writing it into the user's `.git/index` would race their own
//! `git add` for `index.lock`. A `git status` the user runs in the default
//! `normal` mode would also discard a cache recorded in `all` mode, so the next
//! query pays the full walk plus a rewrite. The cache therefore lives in a
//! private copy of the index under ecp's cache root, one directory per
//! worktree gitdir, and git reads that copy through `GIT_INDEX_FILE`.
//!
//! Every condition the copy cannot serve exactly takes the plain command, so
//! the answer never comes from a stale or empty index.

use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::UNIX_EPOCH;

use xxhash_rust::xxh3::xxh3_64;

use crate::git::safe_exec;

const STATUS_ARGS: [&str; 4] = ["status", "--porcelain", "-z", "--untracked-files=all"];

/// git uses the untracked cache only when the mode it recorded matches the
/// effective `status.showUntrackedFiles`, and the `-uall` flag alone leaves
/// that config at `normal`. `core.splitIndex=false` stops git from splitting
/// the private copy, which would put a `sharedindex.*` file in the user's
/// gitdir.
const UNTRACKED_CACHE_CONFIG: [&str; 6] = [
    "-c",
    "core.untrackedCache=true",
    "-c",
    "status.showUntrackedFiles=all",
    "-c",
    "core.splitIndex=false",
];

const PRIVATE_INDEX_DIR: &str = "git-index";
const LOCK_FILE: &str = "lock";
const FAILED_SUFFIX: &str = ".failed";

/// The output of `git status --porcelain -z --untracked-files=all` in
/// `worktree`, byte for byte.
pub fn porcelain_all(worktree: &Path) -> io::Result<Output> {
    let cache_root = ecp_core::registry::resolve_home_ecp().join(PRIVATE_INDEX_DIR);
    porcelain_all_in(worktree, &cache_root)
}

fn porcelain_all_in(worktree: &Path, cache_root: &Path) -> io::Result<Output> {
    if let Some(out) = status_on_private_index(worktree, cache_root) {
        return Ok(out);
    }
    plain(worktree)
}

fn plain(worktree: &Path) -> io::Result<Output> {
    safe_exec::git()
        .args(STATUS_ARGS)
        .current_dir(worktree)
        .output()
}

/// The worktree's own gitdir when the private index may serve `worktree`.
///
/// A caller's `GIT_INDEX_FILE` (ecp run from a git hook) names the index the
/// plain command must read, so it is honoured by not overriding it. The file
/// readers behind `git_dirs` decline env overrides and unmodelled layouts
/// (bare repositories among them).
fn eligible_gitdir(worktree: &Path) -> Option<PathBuf> {
    if std::env::var_os("GIT_INDEX_FILE").is_some() || !dir_mtime_reliable(worktree) {
        return None;
    }
    crate::git_cache::git_dirs(worktree).map(|(gitdir, _)| gitdir)
}

/// Runs status on the private copy, or `None` for the caller to run the plain
/// command.
///
/// Copies are named by the real index's identity, so a copy never changes
/// meaning once written and a newer real index gets a new name. Every reader
/// holds a shared lock from choosing a copy until git exits; pruning old
/// copies takes the exclusive lock without waiting. A copy unlinked while git
/// opens it reads as an empty index and lists every tracked file as deleted,
/// with success, and the lock is what rules that out.
fn status_on_private_index(worktree: &Path, cache_root: &Path) -> Option<Output> {
    let gitdir = eligible_gitdir(worktree)?;
    let dir = cache_root.join(format!(
        "{:016x}",
        xxh3_64(gitdir.as_os_str().as_encoded_bytes())
    ));
    fs::create_dir_all(&dir).ok()?;
    let lock = lock_file(&dir)?;
    lock.lock_shared().ok()?;

    let mut real = File::open(gitdir.join("index")).ok()?;
    let real_meta = real.metadata().ok()?;
    let name = copy_name(&mut real, &real_meta)?;
    let copy = dir.join(&name);
    if fs::symlink_metadata(failed_marker(&copy)).is_ok() {
        return None;
    }
    let created = fs::symlink_metadata(&copy).is_err();
    if created {
        // The index carries a `link` extension only while a split index's
        // `sharedindex.*` sits beside it, and git resolves that file from the
        // gitdir the copy does not live in.
        if split_index_present(&gitdir) {
            return None;
        }
        write_copy(&mut real, &real_meta, &copy).ok()?;
    }
    drop(real);

    let out = safe_exec::git()
        .args(UNTRACKED_CACHE_CONFIG)
        .args(STATUS_ARGS)
        .env("GIT_INDEX_FILE", &copy)
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        // Marked rather than deleted: deleting would need the exclusive lock,
        // and the marker keeps later queries on this index off a copy git has
        // already refused once.
        let _ = File::create(failed_marker(&copy));
        return None;
    }
    drop(lock);
    if created {
        prune(&dir, &name);
    }
    Some(out)
}

fn lock_file(dir: &Path) -> Option<File> {
    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .ok()
}

fn failed_marker(copy: &Path) -> PathBuf {
    let mut name = copy.as_os_str().to_owned();
    name.push(FAILED_SUFFIX);
    PathBuf::from(name)
}

/// The real index's identity: mtime, size and inode, plus the trailing
/// checksum git writes over the content. The checksum alone is not enough
/// because `index.skipHash` writes it as zeros; the stat triple alone is not
/// enough because a reused inode written within one timestamp tick at the same
/// size would collide.
fn copy_name(real: &mut File, meta: &fs::Metadata) -> Option<String> {
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut checksum = [0u8; 20];
    real.seek(SeekFrom::End(-(checksum.len() as i64))).ok()?;
    real.read_exact(&mut checksum).ok()?;
    real.rewind().ok()?;
    Some(format!(
        "index-{mtime_ns:x}-{:x}-{:x}-{}",
        meta.len(),
        inode(meta),
        hex::encode(checksum)
    ))
}

#[cfg(unix)]
fn inode(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn inode(_meta: &fs::Metadata) -> u64 {
    0
}

fn split_index_present(gitdir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(gitdir) else {
        return true;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .as_encoded_bytes()
            .starts_with(b"sharedindex.")
    })
}

/// Copies `real` to `copy` through a temporary name, so a reader sees either
/// no copy or a whole one. The real index's mtime is carried over: git treats
/// an entry whose mtime is not older than the index file as racily clean and
/// compares its content, and a fresh mtime on the copy would trust entries the
/// real index would have re-read.
fn write_copy(real: &mut File, real_meta: &fs::Metadata, copy: &Path) -> io::Result<()> {
    let mut tmp_name = copy.as_os_str().to_owned();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let written = (|| -> io::Result<()> {
        let mut dst = File::create(&tmp)?;
        io::copy(real, &mut dst)?;
        dst.set_times(FileTimes::new().set_modified(real_meta.modified()?))?;
        drop(dst);
        fs::rename(&tmp, copy)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Removes every copy but `keep`, when no reader holds the lock. A busy lock
/// skips the prune; the next copy written prunes instead.
fn prune(dir: &Path, keep: &str) {
    let Some(lock) = lock_file(dir) else {
        return;
    };
    if lock.try_lock().is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != LOCK_FILE && !name.starts_with(keep) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The untracked cache trusts a directory's mtime to change whenever an entry
/// is added to or removed from it. Local POSIX filesystems guarantee that;
/// network mounts, 9p (WSL's `/mnt/c`), FUSE and overlay do not reliably, and
/// a missed bump there hides a new file. `git update-index
/// --test-untracked-cache` answers the same question, but it sleeps about six
/// seconds and creates `mtime-test-*` in the worktree root, where a concurrent
/// status lists it. The filesystem type is read instead, and anything not
/// known to be local takes the plain command.
#[cfg(target_os = "linux")]
fn dir_mtime_reliable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const EXT234: u32 = 0xEF53;
    const XFS: u32 = 0x5846_5342;
    const BTRFS: u32 = 0x9123_683E;
    const TMPFS: u32 = 0x0102_1994;
    const F2FS: u32 = 0xF2F5_2010;
    const ZFS: u32 = 0x2FC1_2FC1;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `buf` is only
    // read after statfs reports that it filled it.
    if unsafe { libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs returned 0, so it initialised `buf`.
    let f_type = unsafe { buf.assume_init() }.f_type;
    // `f_type`'s width differs across targets; every magic fits in 32 bits.
    #[allow(clippy::unnecessary_cast)]
    let magic = f_type as u32;
    matches!(magic, EXT234 | XFS | BTRFS | TMPFS | F2FS | ZFS)
}

#[cfg(target_os = "macos")]
fn dir_mtime_reliable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `buf` is only
    // read after statfs reports that it filled it.
    if unsafe { libc::statfs(c_path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs returned 0, so it initialised `buf`, and `f_fstypename`
    // is a NUL-terminated array inside it.
    let buf = unsafe { buf.assume_init() };
    let kind = unsafe { std::ffi::CStr::from_ptr(buf.f_fstypename.as_ptr()) };
    matches!(kind.to_bytes(), b"apfs" | b"hfs")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn dir_mtime_reliable(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        repo: PathBuf,
        cache: PathBuf,
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = safe_exec::git()
            .args([
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    /// A committed repo with one tracked file at the root and one in `src/`.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        write(&repo, "lib.rs", "pub fn a() {}\n");
        write(&repo, "src/b.rs", "pub fn b() {}\n");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        Fixture {
            _tmp: tmp,
            repo,
            cache: root.join("cache"),
        }
    }

    /// The copies in the cache, excluding locks, markers and temporaries.
    fn copies(cache: &Path) -> Vec<String> {
        let Ok(dirs) = fs::read_dir(cache) else {
            return Vec::new();
        };
        let mut names: Vec<String> = dirs
            .flatten()
            .flat_map(|d| fs::read_dir(d.path()).unwrap().flatten())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("index-") && !n.contains('.'))
            .collect();
        names.sort();
        names
    }

    fn markers(cache: &Path) -> usize {
        let Ok(dirs) = fs::read_dir(cache) else {
            return 0;
        };
        dirs.flatten()
            .flat_map(|d| fs::read_dir(d.path()).unwrap().flatten())
            .filter(|e| e.file_name().to_string_lossy().ends_with(FAILED_SUFFIX))
            .count()
    }

    /// Runs the helper twice (the first run writes the untracked cache, the
    /// second reads it) and requires both to equal the plain command.
    fn assert_matches_plain(worktree: &Path, cache: &Path) -> Vec<u8> {
        let expected = plain(worktree).unwrap();
        for run in 0..2 {
            let got = porcelain_all_in(worktree, cache).unwrap();
            assert_eq!(
                got.status.code(),
                expected.status.code(),
                "run {run}: exit code"
            );
            assert_eq!(
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&expected.stdout),
                "run {run}: porcelain"
            );
        }
        expected.stdout
    }

    fn fast_path_expected(repo: &Path) -> bool {
        eligible_gitdir(repo).is_some()
    }

    /// Stops the real index from changing. While a tracked file's mtime is not
    /// older than the index (git may compare whole seconds), every status
    /// rewrites `.git/index` to settle the racily clean entry, which renames
    /// the copy a test is about to tamper with. Backdating the files and
    /// refreshing once ends that without depending on the clock.
    fn settle(repo: &Path) {
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
        for rel in ["lib.rs", "src/b.rs"] {
            File::options()
                .write(true)
                .open(repo.join(rel))
                .unwrap()
                .set_times(FileTimes::new().set_modified(past))
                .unwrap();
        }
        git(repo, &["update-index", "-q", "--refresh"]);
    }

    #[test]
    fn test_porcelain_all_clean_repo_matches_plain() {
        let f = fixture();
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert!(out.is_empty());
        if fast_path_expected(&f.repo) {
            assert_eq!(copies(&f.cache).len(), 1, "fast path did not engage");
            assert_eq!(markers(&f.cache), 0);
        }
    }

    #[test]
    fn test_porcelain_all_modified_tracked_file_matches_plain() {
        let f = fixture();
        assert_matches_plain(&f.repo, &f.cache);
        write(&f.repo, "src/b.rs", "pub fn b2() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b" M src/b.rs\0");
    }

    #[test]
    fn test_porcelain_all_new_untracked_file_matches_plain() {
        let f = fixture();
        assert_matches_plain(&f.repo, &f.cache);
        write(&f.repo, "new file é.rs", "pub fn n() {}\n");
        write(&f.repo, "src/c.rs", "pub fn c() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, "?? new file é.rs\0?? src/c.rs\0".as_bytes());
    }

    #[test]
    fn test_porcelain_all_new_nested_dir_matches_plain() {
        let f = fixture();
        assert_matches_plain(&f.repo, &f.cache);
        write(&f.repo, "pkg/deep/er/mod.rs", "pub fn m() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b"?? pkg/deep/er/mod.rs\0");
    }

    #[test]
    fn test_porcelain_all_gitignore_toggle_matches_plain() {
        let f = fixture();
        write(&f.repo, "scratch/tmp.rs", "pub fn t() {}\n");
        assert_matches_plain(&f.repo, &f.cache);
        write(&f.repo, ".gitignore", "scratch/\n");
        git(&f.repo, &["add", ".gitignore"]);
        git(&f.repo, &["commit", "-qm", "ignore"]);
        let ignored = assert_matches_plain(&f.repo, &f.cache);
        assert!(ignored.is_empty(), "{}", String::from_utf8_lossy(&ignored));
        write(&f.repo, ".gitignore", "");
        let unignored = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(unignored, b" M .gitignore\0?? scratch/tmp.rs\0");
    }

    #[test]
    fn test_porcelain_all_info_exclude_matches_plain() {
        let f = fixture();
        write(&f.repo, "local/x.rs", "pub fn x() {}\n");
        assert_matches_plain(&f.repo, &f.cache);
        write(&f.repo, ".git/info/exclude", "local/\n");
        let excluded = assert_matches_plain(&f.repo, &f.cache);
        assert!(excluded.is_empty());
        write(&f.repo, ".git/info/exclude", "");
        let included = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(included, b"?? local/x.rs\0");
    }

    #[test]
    fn test_porcelain_all_after_git_add_refreshes_copy() {
        let f = fixture();
        write(&f.repo, "added.rs", "pub fn d() {}\n");
        let before = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(before, b"?? added.rs\0");
        let first_copy = copies(&f.cache);
        git(&f.repo, &["add", "added.rs"]);
        let after = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(after, b"A  added.rs\0");
        if fast_path_expected(&f.repo) {
            let fresh: Vec<String> = copies(&f.cache)
                .into_iter()
                .filter(|c| !first_copy.contains(c))
                .collect();
            assert_eq!(fresh.len(), 1, "copy not refreshed");
            // A sibling test thread spawning git holds a duplicate of the lock
            // fd until its exec, so the inline prune may have been skipped as
            // designed; once the lock is free, the old copy must go.
            let dir = fs::read_dir(&f.cache)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            for _ in 0..100 {
                if copies(&f.cache) == fresh {
                    break;
                }
                prune(&dir, &fresh[0]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(copies(&f.cache), fresh, "old copy not pruned");
        }
    }

    #[test]
    fn test_porcelain_all_linked_worktree_matches_plain() {
        let f = fixture();
        let wt = f.repo.parent().unwrap().join("wt2");
        git(&f.repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        write(&wt, "only_in_wt.rs", "pub fn w() {}\n");
        write(&f.repo, "only_in_main.rs", "pub fn m() {}\n");
        let main_out = assert_matches_plain(&f.repo, &f.cache);
        let wt_out = assert_matches_plain(&wt, &f.cache);
        assert_eq!(main_out, b"?? only_in_main.rs\0");
        assert_eq!(wt_out, b"?? only_in_wt.rs\0");
        if fast_path_expected(&wt) {
            assert_eq!(copies(&f.cache).len(), 2, "one copy per worktree");
        }
    }

    #[test]
    fn test_porcelain_all_split_index_falls_back() {
        let f = fixture();
        git(&f.repo, &["update-index", "--split-index"]);
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        write(&f.repo, "lib.rs", "pub fn a2() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b" M lib.rs\0?? new.rs\0");
        assert!(
            copies(&f.cache).is_empty(),
            "split index must not be copied"
        );
    }

    #[test]
    fn test_porcelain_all_corrupt_copy_falls_back() {
        let f = fixture();
        settle(&f.repo);
        assert_matches_plain(&f.repo, &f.cache);
        if !fast_path_expected(&f.repo) {
            return;
        }
        let name = copies(&f.cache).pop().unwrap();
        let dir = fs::read_dir(&f.cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(dir.join(&name), b"garbage").unwrap();
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b"?? new.rs\0");
        assert_eq!(markers(&f.cache), 1, "failed copy not marked");
    }

    #[test]
    fn test_porcelain_all_missing_copy_recreates() {
        let f = fixture();
        settle(&f.repo);
        assert_matches_plain(&f.repo, &f.cache);
        if !fast_path_expected(&f.repo) {
            return;
        }
        let name = copies(&f.cache).pop().unwrap();
        let dir = fs::read_dir(&f.cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::remove_file(dir.join(&name)).unwrap();
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b"?? new.rs\0");
        assert_eq!(copies(&f.cache), vec![name]);
    }

    #[test]
    fn test_porcelain_all_repo_without_index_matches_plain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        write(&repo, "a.rs", "pub fn a() {}\n");
        let out = assert_matches_plain(&repo, &root.join("cache"));
        assert_eq!(out, b"?? a.rs\0");
    }

    #[test]
    fn test_porcelain_all_not_a_repo_matches_plain_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(tmp.path()).unwrap();
        let got = porcelain_all_in(&dir, &dir.join("cache")).unwrap();
        let expected = plain(&dir).unwrap();
        assert_eq!(got.status.code(), expected.status.code());
        assert_eq!(got.stdout, expected.stdout);
    }

    #[test]
    fn test_porcelain_all_unwritable_cache_root_falls_back() {
        let f = fixture();
        fs::write(&f.cache, b"a file, not a directory").unwrap();
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        let out = assert_matches_plain(&f.repo, &f.cache);
        assert_eq!(out, b"?? new.rs\0");
    }
}
