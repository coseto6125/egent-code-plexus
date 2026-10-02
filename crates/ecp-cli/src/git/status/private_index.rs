//! The private index behind [`super::porcelain_all`] on Linux.
//!
//! Most of the status cost is the untracked-file walk. git can skip unchanged
//! directories through its untracked cache, but that cache lives inside the
//! index, and writing it into the user's `.git/index` would race their own
//! `git add` for `index.lock`. A `git status` the user runs in the default
//! `normal` mode would also discard a cache recorded in `all` mode, so the next
//! query pays the full walk plus a rewrite. The cache therefore lives in a
//! private copy of the index under ecp's cache root, one directory per
//! worktree gitdir, and git reads that copy through `GIT_INDEX_FILE`.
//!
//! Every condition the copy cannot serve exactly takes the plain command, so
//! the answer never comes from a stale or empty index. Only Linux qualifies:
//! elsewhere neither the mounts below a worktree nor the system config file
//! git reads can be named without asking git.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xxhash_rust::xxh3::xxh3_64;

use super::{plain_in, with_env, EnvOverrides, PRIVATE_INDEX_DIR, STATUS_ARGS};
use crate::git::safe_exec;

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

/// A caller's `GIT_INDEX_FILE` (ecp run from a git hook) names the index the
/// plain command must read, so it is honoured by not overriding it. Config
/// passed through the environment is invisible to the config identity, and
/// `GIT_CONFIG_NOSYSTEM` drops a file the identity reads. The last four
/// redirect git away from the on-disk layout `git_layout_unchecked` reads;
/// they are checked here, through the env seam, rather than in the process env.
const BLOCKING_ENV: [&str; 9] = [
    "GIT_INDEX_FILE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_ATTR_NOSYSTEM",
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_CEILING_DIRECTORIES",
];

const LOCK_FILE: &str = "lock";
const GITDIR_RECORD: &str = "gitdir";
const COPY_PREFIX: &str = "index-";
const FAILED_SUFFIX: &str = ".failed";
const MOUNTINFO: &str = "/proc/self/mountinfo";

/// The private copy's answer for `worktree`, or `None` for the caller to run
/// the plain command.
pub(super) fn status(worktree: &Path, cache_root: &Path, env: &EnvOverrides) -> Option<Output> {
    let repo = eligible(worktree, env)?;
    // git resolves `GIT_INDEX_FILE` against the worktree it runs in, not
    // ecp's cwd, and a path that names no file reads as an empty index with
    // success.
    let root = std::path::absolute(cache_root).ok()?;
    status_on_private_index(worktree, &repo, &root)
}

/// What the private copy needs from a worktree that passed every check.
struct Repo<'a> {
    gitdir: PathBuf,
    top: PathBuf,
    git: PathBuf,
    config_files: Vec<PathBuf>,
    attribute_files: Vec<PathBuf>,
    config_id: u64,
    env: &'a EnvOverrides,
}

/// The worktree's repository when the private index may serve `worktree`.
/// The checks run cheapest and likeliest to decline first.
///
/// The layout reader declines unmodelled layouts (bare repositories among
/// them).
fn eligible<'a>(worktree: &Path, env: &'a EnvOverrides) -> Option<Repo<'a>> {
    let var = |key: &str| env_var(env, key);
    if BLOCKING_ENV.iter().any(|key| var(key).is_some()) {
        return None;
    }
    let (top, gitdir, common) = crate::git_cache::git_layout_unchecked(worktree)?;
    if !dir_mtime_reliable(&top) || !no_mount_below(fs::read(MOUNTINFO), &top) {
        return None;
    }
    let git = git_on_path(&var("PATH")?)?;
    let config_files = config_files(&gitdir, &common, &git, &var)?;
    let attribute_files = attribute_files(&common, &var)?;
    let config_id = config_identity(&config_files, &attribute_files)?;
    Some(Repo {
        gitdir,
        top,
        git,
        config_files,
        attribute_files,
        config_id,
        env,
    })
}

/// `key` as the git this module spawns sees it.
fn env_var(env: &EnvOverrides, key: &str) -> Option<OsString> {
    match env.iter().rev().find(|(k, _)| k == key) {
        Some((_, value)) => value.clone(),
        None => std::env::var_os(key),
    }
}

/// The `git` a `Command` would run. The fast path spawns this binary by its
/// path, so the system config file named for it is the one that git reads,
/// and the `PATH` walk is paid once.
fn git_on_path(path: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        // A relative entry resolves against the child's cwd, not this one.
        if !dir.is_absolute() {
            return None;
        }
        let candidate = dir.join("git");
        if fs::metadata(&candidate)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return Some(candidate);
        }
    }
    None
}

/// Every config file git reads for this worktree. The untracked cache does
/// not record settings such as `core.ignorecase`, so a cache built under one
/// config would keep answering under another; the files' content is part of
/// the copy's name instead. A relative path resolves against the worktree in
/// git but against ecp's cwd here, so any relative one declines.
fn config_files(
    gitdir: &Path,
    common: &Path,
    git: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<Vec<PathBuf>> {
    let system = match env("GIT_CONFIG_SYSTEM") {
        Some(path) => PathBuf::from(path),
        // Distribution packages build git with `/etc` as its sysconfdir. Any
        // other build may read a system file that only git itself can name.
        None if git
            .parent()
            .is_some_and(|dir| dir == Path::new("/usr/bin") || dir == Path::new("/bin")) =>
        {
            PathBuf::from("/etc/gitconfig")
        }
        None => return None,
    };
    let mut files = vec![
        system,
        common.join("config"),
        gitdir.join("config.worktree"),
    ];
    match env("GIT_CONFIG_GLOBAL") {
        Some(path) => files.push(PathBuf::from(path)),
        None => {
            let home = env("HOME").map(PathBuf::from);
            let xdg = env("XDG_CONFIG_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|h| h.join(".config")));
            files.extend(xdg.map(|dir| dir.join("git").join("config")));
            files.extend(home.map(|h| h.join(".gitconfig")));
        }
    }
    files.iter().all(|file| file.is_absolute()).then_some(files)
}

/// The attribute files git reads outside the tree. git normalises content by
/// attributes, so an entry the copy re-verified under one set stays trusted
/// after the set changes; their content is part of the copy's name too. A
/// custom `core.attributesFile` is not modelled (see `blocks_copy`). Relative
/// paths decline as in [`config_files`].
fn attribute_files(common: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Option<Vec<PathBuf>> {
    let mut files = vec![common.join("info").join("attributes")];
    let home = env("HOME").map(PathBuf::from);
    let xdg = env("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")));
    files.extend(xdg.map(|dir| dir.join("git").join("attributes")));
    // `config_files` already declined any git whose sysconfdir is not `/etc`.
    files.push(PathBuf::from("/etc/gitattributes"));
    files.iter().all(|file| file.is_absolute()).then_some(files)
}

/// A digest of the config and attribute files' content, absent files
/// included. `None` when a file cannot be read or a config file holds config
/// the copy cannot honour. Attribute files are free-form patterns, so only
/// config files go through `blocks_copy`.
fn config_identity(config: &[PathBuf], attributes: &[PathBuf]) -> Option<u64> {
    let mut seen = Vec::new();
    let files = config
        .iter()
        .map(|file| (file, true))
        .chain(attributes.iter().map(|file| (file, false)));
    for (file, is_config) in files {
        match fs::read(file) {
            Ok(body) if !(is_config && blocks_copy(&body)) => {
                seen.push(1);
                seen.extend_from_slice(&(body.len() as u64).to_le_bytes());
                seen.extend_from_slice(&body);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => seen.push(0),
            _ => return None,
        }
    }
    Some(xxh3_64(&seen))
}

/// Config the copy cannot honour:
/// - an include names a file the identity does not read;
/// - `core.worktree` moves the tree away from the directory checked for
///   mounts;
/// - under sparse checkout, git clears skip-worktree on paths present in the
///   tree and writes that into the copy, while the real index keeps the bit;
/// - with `core.trustctime` or `core.checkStat` relaxed, an entry the copy
///   re-verified stays trusted where the real index still compares content;
/// - `extensions.objectFormat` may widen the object names past the 20 bytes
///   the gitlink scan steps over;
/// - `core.attributesFile` names an attributes file the identity does not read.
///
/// Keys are matched by name in any section, which only ever over-matches. git
/// skips a UTF-8 byte order mark at the start of the file.
fn blocks_copy(config: &[u8]) -> bool {
    let text = String::from_utf8_lossy(config);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        let rest = match line.strip_prefix('[') {
            Some(header) if header.starts_with("include") => return true,
            Some(header) => header.split_once(']').map_or("", |(_, rest)| rest),
            None => line.as_str(),
        };
        let key = rest
            .trim_start()
            .split(|c: char| c == '=' || c.is_ascii_whitespace())
            .next()
            .unwrap_or("");
        matches!(
            key,
            "worktree"
                | "sparsecheckout"
                | "sparsecheckoutcone"
                | "trustctime"
                | "checkstat"
                | "objectformat"
                | "attributesfile"
        )
    })
}

/// True when `mountinfo` was read and lists no mount point strictly below
/// `top`. The filesystem check covers only the mount holding `top`; a FUSE or
/// network mount further down misses directory mtime bumps the same way.
fn no_mount_below(mountinfo: io::Result<Vec<u8>>, top: &Path) -> bool {
    let Ok(text) = mountinfo else {
        return false;
    };
    let top = top.as_os_str().as_encoded_bytes();
    let mut prefix = top.to_vec();
    if !prefix.ends_with(b"/") {
        prefix.push(b'/');
    }
    !text
        .split(|&b| b == b'\n')
        .filter_map(|line| line.split(|&b| b == b' ').nth(4))
        .map(unescape_mount_point)
        .any(|point| point != top && point.starts_with(&prefix))
}

/// The kernel writes space, tab, newline and backslash in a mount point as
/// three octal digits after a backslash.
fn unescape_mount_point(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field;
    while let Some((&byte, tail)) = rest.split_first() {
        let (decoded, consumed) = match (byte, tail) {
            (b'\\', &[a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7', ..]) => {
                (((a - b'0') << 6) | ((b - b'0') << 3) | (c - b'0'), 3)
            }
            _ => (byte, 0),
        };
        out.push(decoded);
        rest = &tail[consumed..];
    }
    out
}

/// The directory holding `gitdir`'s copies under `cache_root`.
fn copy_dir(cache_root: &Path, gitdir: &str) -> PathBuf {
    cache_root.join(format!("{:016x}", xxh3_64(gitdir.as_bytes())))
}

/// Runs status on the private copy, or `None` for the caller to run the plain
/// command.
///
/// Copies are named by the real index's identity and the config's, so a copy
/// never changes meaning once written and a newer real index or config gets a
/// new name. One run at a time holds the directory's lock from choosing a
/// copy until it has stamped and pruned; a copy unlinked while git opens it
/// reads as an empty index and lists every tracked file as deleted, with
/// success, and the lock is what rules that out.
fn status_on_private_index(worktree: &Path, repo: &Repo, cache_root: &Path) -> Option<Output> {
    let gitdir = repo.gitdir.to_str()?;
    let dir = copy_dir(cache_root, gitdir);
    fs::create_dir_all(&dir).ok()?;
    // A walk that reaches the copies lists git's transient `<copy>.lock`.
    if fs::canonicalize(&dir).ok()?.starts_with(&repo.top) {
        return None;
    }
    let lock = lock_dir(&dir)?;

    let mut real = File::open(repo.gitdir.join("index")).ok()?;
    let real_meta = real.metadata().ok()?;
    let real_mtime = real_meta.modified().ok()?;
    let name = format!(
        "{}-{:016x}",
        copy_name(&mut real, &real_meta)?,
        repo.config_id
    );
    let copy = dir.join(&name);
    if fs::symlink_metadata(marker(&copy, FAILED_SUFFIX)).is_ok() {
        return None;
    }
    // A run that ended between git's rewrite of the copy and the stamp left
    // git's write time on it, which keeps a same-second change hidden. Such a
    // run also left the lock file one byte long: resizing the open lock marks
    // the window without creating or unlinking a file on every query.
    if lock.metadata().ok()?.len() != 0
        && fs::remove_file(&copy).is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
    {
        return None;
    }
    let created = fs::symlink_metadata(&copy).is_err();
    if created {
        // The index carries a `link` extension only while a split index's
        // `sharedindex.*` sits beside it, and git resolves that file from the
        // gitdir the copy does not live in.
        if split_index_present(&repo.gitdir) {
            return None;
        }
        fs::write(dir.join(GITDIR_RECORD), gitdir).ok()?;
        let mut body = Vec::new();
        real.read_to_end(&mut body).ok()?;
        // git passes the `-c` settings on to the status it runs in each
        // submodule, where they override the submodule's own config.
        if !gitlink_free(&body) {
            let _ = File::create(marker(&copy, FAILED_SUFFIX));
            prune(&dir, &name);
            return None;
        }
        write_copy(&body, real_mtime, &copy).ok()?;
    }
    drop(real);

    lock.set_len(1).ok()?;
    let started = SystemTime::now();
    let out = with_env(&mut safe_exec::git_at(&repo.git), repo.env)
        .args(UNTRACKED_CACHE_CONFIG)
        .args(STATUS_ARGS)
        .env("GIT_INDEX_FILE", &copy)
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        let plain = plain_in(worktree, repo.env).ok()?;
        // Marked rather than retried: the marker keeps later queries on this
        // index off a copy git has refused once. A git killed by a signal, or
        // a repository the plain command refuses too, says nothing against
        // the copy.
        if out.status.code().is_some() && plain.status.success() {
            let _ = File::create(marker(&copy, FAILED_SUFFIX));
        }
        return Some(plain);
    }
    let config_held =
        config_identity(&repo.config_files, &repo.attribute_files) == Some(repo.config_id);
    if config_held && stamp_before(&copy, started, real_mtime) {
        let _ = lock.set_len(0);
    } else {
        // The lock stays marked, so the next run discards a copy this one
        // failed to remove.
        let _ = fs::remove_file(&copy);
    }
    if created {
        prune(&dir, &name);
    }
    // A config that changed while git ran may have met a cache built under
    // the old one, so this run's answer is not trusted either.
    config_held.then_some(out)
}

/// Takes `dir`'s lock without waiting. One run at a time chooses, writes,
/// reads and prunes the copies in `dir`; a run that finds the lock held takes
/// the plain command instead of queueing behind it. A sibling thread's spawn
/// briefly holding a duplicate of the lock fd only sends a run to plain.
fn lock_dir(dir: &Path) -> Option<File> {
    let path = dir.join(LOCK_FILE);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .ok()?;
    lock_at_path(lock, &path)
}

/// Takes `lock` while it is still the file at `path`. `ecp admin gc` moves a
/// directory away while holding its lock, so a lock opened before that guards
/// nothing.
fn lock_at_path(lock: File, path: &Path) -> Option<File> {
    lock.try_lock().ok()?;
    (lock.metadata().ok()?.ino() == fs::metadata(path).ok()?.ino()).then_some(lock)
}

fn marker(copy: &Path, suffix: &str) -> PathBuf {
    let mut name = copy.as_os_str().to_owned();
    name.push(suffix);
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
        "{COPY_PREFIX}{mtime_ns:x}-{:x}-{:x}-{}",
        meta.len(),
        meta.ino(),
        hex::encode(checksum)
    ))
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

/// True when the index's entries were walked and none is a gitlink. Entries
/// follow a 12-byte header; each holds stat data with the mode at byte 24, a
/// 20-byte object name, 16 bits of flags (32 with the extended bit), and the
/// path. Versions 2 and 3 NUL-pad the entry to a multiple of eight bytes;
/// version 4 prefixes the path with a varint and ends it with one NUL.
fn gitlink_free(index: &[u8]) -> bool {
    const GITLINK: u32 = 0o160_000;
    const EXTENDED: u16 = 0x4000;
    const FLAGS_AT: usize = 40 + 20;
    let u32_at = |at: usize| {
        let bytes = index.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes(bytes.try_into().ok()?))
    };
    let (Some(b"DIRC"), Some(version @ 2..=4), Some(count)) =
        (index.get(..4), u32_at(4), u32_at(8))
    else {
        return false;
    };
    let mut at = 12;
    for _ in 0..count {
        let (Some(mode), Some(&[hi, lo])) =
            (u32_at(at + 24), index.get(at + FLAGS_AT..at + FLAGS_AT + 2))
        else {
            return false;
        };
        if mode & 0o170_000 == GITLINK {
            return false;
        }
        let mut path = at + FLAGS_AT + 2;
        if u16::from_be_bytes([hi, lo]) & EXTENDED != 0 {
            path += 2;
        }
        if version == 4 {
            while index.get(path).is_some_and(|b| b & 0x80 != 0) {
                path += 1;
            }
            path += 1;
        }
        let Some(len) = index
            .get(path..)
            .and_then(|rest| rest.iter().position(|&b| b == 0))
        else {
            return false;
        };
        at = match version {
            4 => path + len + 1,
            _ => at + ((path - at + len + 8) & !7),
        };
    }
    true
}

/// Writes the real index's bytes to `copy` through a temporary name, so a
/// crash leaves either no copy or a whole one. The real index's mtime is
/// carried over: git treats an entry whose mtime is not older than the index
/// file as racily clean and compares its content, and a fresh mtime on the
/// copy would trust entries the real index would have re-read.
fn write_copy(body: &[u8], real_mtime: SystemTime, copy: &Path) -> io::Result<()> {
    let tmp = marker(copy, ".tmp");
    let written = (|| -> io::Result<()> {
        let mut dst = File::create(&tmp)?;
        dst.write_all(body)?;
        dst.set_times(FileTimes::new().set_modified(real_mtime))?;
        // git does not verify the index checksum on read, so a copy truncated by
        // power loss on an entry boundary reads as valid with wrong output.
        dst.sync_all()?;
        drop(dst);
        fs::rename(&tmp, copy)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Stamps `copy` between the real index's mtime and the later of that mtime
/// and one second before git started, and returns whether it holds such a
/// stamp.
///
/// git stamps a copy it rewrites with the write time and trusts a directory or
/// entry whose recorded mtime is older than that stamp, in whole seconds
/// unless git was built with nanosecond stamps. A file landing in a directory
/// in the same second git stat'd it, with the write in the next second, would
/// then stay hidden until the directory changes again. Every stat this run
/// recorded is at or after `started` and after the real index was written, so
/// the ceiling keeps them racy. The floor keeps every entry the real index
/// trusts trusted in the copy too: a racy entry is compared by content, and
/// a `text` or `eol` attribute over a blob stored with CRLF reads as modified
/// even though plain status trusts its stat. A real index stamped after
/// `started` bounds nothing, and the copy is not kept.
fn stamp_before(copy: &Path, started: SystemTime, real_mtime: SystemTime) -> bool {
    let Some(ceiling) = started.checked_sub(Duration::from_secs(1)) else {
        return false;
    };
    if real_mtime > started {
        return false;
    }
    let ceiling = ceiling.max(real_mtime);
    match fs::metadata(copy).and_then(|m| m.modified()) {
        Ok(stamp) if (real_mtime..=ceiling).contains(&stamp) => true,
        Ok(stamp) => File::options()
            .write(true)
            .open(copy)
            .and_then(|f| {
                f.set_times(FileTimes::new().set_modified(stamp.clamp(real_mtime, ceiling)))
            })
            .is_ok(),
        Err(_) => false,
    }
}

/// Removes every copy but `keep`. The caller holds the directory's lock, so
/// no run is reading one.
fn prune(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(COPY_PREFIX) && !name.starts_with(keep) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Removes each copy directory under `home_ecp` whose recorded gitdir is gone,
/// and returns how many went. A missing cache root holds none. A directory
/// without a record never got a copy. A directory whose lock is held is in use
/// and stays.
pub fn sweep_orphans(home_ecp: &Path) -> io::Result<usize> {
    static TOMBSTONES: AtomicU64 = AtomicU64::new(0);
    let root = home_ecp.join(PRIVATE_INDEX_DIR);
    let dirs = match fs::read_dir(&root) {
        Ok(dirs) => dirs,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    Ok(dirs
        .flatten()
        .filter(|entry| {
            let dir = entry.path();
            let Some(_lock) = lock_dir(&dir) else {
                return false;
            };
            let orphan = match fs::read_to_string(dir.join(GITDIR_RECORD)) {
                Ok(gitdir) => {
                    fs::symlink_metadata(gitdir).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
                }
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            };
            if !orphan {
                return false;
            }
            // Deleted in place, the directory loses its lock file first, and a
            // query could lock a fresh one there while the rest is still being
            // deleted. Moved away under the lock, it is out of every query's
            // reach.
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let tombstone = root.join(format!(
                "{}.gc-{}-{}",
                name.split('.').next().unwrap_or_default(),
                std::process::id(),
                TOMBSTONES.fetch_add(1, Ordering::Relaxed)
            ));
            fs::rename(&dir, &tombstone).is_ok() && fs::remove_dir_all(&tombstone).is_ok()
        })
        .count())
}

/// The untracked cache trusts a directory's mtime to change whenever an entry
/// is added to or removed from it. Local POSIX filesystems guarantee that;
/// network mounts, 9p (WSL's `/mnt/c`), FUSE and overlay do not reliably, and
/// a missed bump there hides a new file. `git update-index
/// --test-untracked-cache` answers the same question, but it sleeps about six
/// seconds and creates `mtime-test-*` in the worktree root, where a concurrent
/// status lists it. The filesystem type is read instead, and anything not
/// known to be local takes the plain command.
fn dir_mtime_reliable(path: &Path) -> bool {
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
    reliable_magic(magic)
}

/// Linux `statfs` magics of the local filesystems that bump a directory's
/// mtime on every entry change.
fn reliable_magic(magic: u32) -> bool {
    const EXT234: u32 = 0xEF53;
    const XFS: u32 = 0x5846_5342;
    const BTRFS: u32 = 0x9123_683E;
    const TMPFS: u32 = 0x0102_1994;
    const F2FS: u32 = 0xF2F5_2010;
    const ZFS: u32 = 0x2FC1_2FC1;
    matches!(magic, EXT234 | XFS | BTRFS | TMPFS | F2FS | ZFS)
}

#[cfg(test)]
mod tests {
    use super::super::porcelain_all_in;
    use super::*;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        repo: PathBuf,
        cache: PathBuf,
    }

    /// The environment every git in these tests runs under, the plain
    /// command's included: the process's own without `GIT_*` (a git hook
    /// running the suite exports `GIT_DIR`, `GIT_INDEX_FILE` and friends) or
    /// `XDG_CONFIG_HOME`, and with `HOME` and the system and global config
    /// files naming nothing, so the developer's config reaches neither the
    /// fixtures nor the answers.
    fn isolated_env() -> Vec<(OsString, Option<OsString>)> {
        const NO_HOME: &str = "/nonexistent";
        let removed = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| key.as_encoded_bytes().starts_with(b"GIT_") || key == "XDG_CONFIG_HOME")
            .map(|key| (key, None));
        let set = [
            ("HOME", NO_HOME.to_owned()),
            ("GIT_CONFIG_SYSTEM", format!("{NO_HOME}/gitconfig")),
            ("GIT_CONFIG_GLOBAL", format!("{NO_HOME}/.gitconfig")),
        ]
        .map(|(key, value)| (key.into(), Some(value.into())));
        removed.chain(set).collect()
    }

    /// Runs git for fixture setup only.
    fn git(dir: &Path, args: &[&str]) {
        let out = with_env(&mut safe_exec::git(), &isolated_env())
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

    /// A committed repo at `<tmp>/<name>` with one tracked file at the root
    /// and one in `src/`.
    fn fixture_named(name: &str) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join(name);
        fs::create_dir_all(&repo).unwrap();
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

    fn fixture() -> Fixture {
        fixture_named("repo")
    }

    /// [`fixture_named`] where the private index may serve. `None`, with the
    /// reason printed, when the temporary directory's filesystem is one whose
    /// directory mtimes the untracked cache cannot trust; any other refusal
    /// fails the test that requires the copy.
    fn served_fixture_named(name: &str) -> Option<Fixture> {
        let f = fixture_named(name);
        if dir_mtime_reliable(&f.repo) {
            return Some(f);
        }
        eprintln!(
            "skipped: {} is on a filesystem the private index declines",
            f.repo.display()
        );
        None
    }

    fn served_fixture() -> Option<Fixture> {
        served_fixture_named("repo")
    }

    fn plain_out(worktree: &Path) -> Output {
        plain_in(worktree, &isolated_env()).unwrap()
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
            .filter(|n| n.starts_with(COPY_PREFIX) && !n.contains('.'))
            .collect();
        names.sort();
        names
    }

    fn only_copy(cache: &Path) -> PathBuf {
        let dirs: Vec<PathBuf> = fs::read_dir(cache)
            .unwrap()
            .flatten()
            .map(|d| d.path())
            .collect();
        assert_eq!(dirs.len(), 1, "one copy directory");
        let names = copies(cache);
        assert_eq!(names.len(), 1, "one copy: {names:?}");
        dirs[0].join(&names[0])
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
        let env = isolated_env();
        let expected = plain_in(worktree, &env).unwrap();
        for run in 0..2 {
            let got = porcelain_all_in(worktree, cache, &env).unwrap();
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

    /// Serves `worktree` from the private copy and requires the plain
    /// command's bytes. A sibling test thread spawning a process holds a
    /// duplicate of the lock fd until its exec, which sends a run to the plain
    /// command by design, so a busy lock is retried rather than failed.
    fn fast(worktree: &Path, cache: &Path) -> Vec<u8> {
        let env = isolated_env();
        serve(worktree, &served_repo(worktree, &env), cache)
    }

    fn served_repo<'a>(worktree: &Path, env: &'a EnvOverrides) -> Repo<'a> {
        eligible(worktree, env).expect("a served fixture must be eligible")
    }

    /// [`fast`] for a worktree already judged eligible.
    fn serve(worktree: &Path, repo: &Repo, cache: &Path) -> Vec<u8> {
        for _ in 0..200 {
            let expected = plain_in(worktree, repo.env).unwrap();
            if let Some(got) = status_on_private_index(worktree, repo, cache) {
                assert_eq!(got.status.code(), expected.status.code());
                assert_eq!(
                    got.stdout,
                    expected.stdout,
                    "{}",
                    String::from_utf8_lossy(&got.stdout)
                );
                return got.stdout;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the private copy never served");
    }

    /// Stops the real index from changing. While a tracked file's mtime is not
    /// older than the index (git may compare whole seconds), every status
    /// rewrites `.git/index` to settle the racily clean entry, which renames
    /// the copy a test is about to tamper with. Backdating the files and
    /// refreshing once ends that without depending on the clock.
    fn settle(repo: &Path) {
        let past = SystemTime::now() - Duration::from_secs(10);
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
    }

    #[test]
    fn test_status_on_private_index_clean_repo_writes_one_copy() {
        let Some(f) = served_fixture() else {
            return;
        };
        assert!(fast(&f.repo, &f.cache).is_empty());
        assert_eq!(copies(&f.cache).len(), 1);
        assert_eq!(markers(&f.cache), 0);
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
    fn test_status_on_private_index_after_git_add_refreshes_and_prunes_copy() {
        let Some(f) = served_fixture() else {
            return;
        };
        write(&f.repo, "added.rs", "pub fn d() {}\n");
        assert_eq!(fast(&f.repo, &f.cache), b"?? added.rs\0");
        let first_copy = copies(&f.cache);
        git(&f.repo, &["add", "added.rs"]);
        assert_eq!(fast(&f.repo, &f.cache), b"A  added.rs\0");
        let now = copies(&f.cache);
        assert_eq!(now.len(), 1, "old copy not pruned: {now:?}");
        assert_ne!(now, first_copy, "copy not refreshed");
    }

    #[test]
    fn test_status_on_private_index_linked_worktree_gets_own_copy() {
        let Some(f) = served_fixture() else {
            return;
        };
        let wt = f.repo.parent().unwrap().join("wt2");
        git(&f.repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        write(&wt, "only_in_wt.rs", "pub fn w() {}\n");
        write(&f.repo, "only_in_main.rs", "pub fn m() {}\n");
        assert_eq!(fast(&f.repo, &f.cache), b"?? only_in_main.rs\0");
        assert_eq!(fast(&wt, &f.cache), b"?? only_in_wt.rs\0");
        assert_eq!(copies(&f.cache).len(), 2, "one copy per worktree");
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
    fn test_status_on_private_index_corrupt_copy_falls_back_and_marks() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        fast(&f.repo, &f.cache);
        fs::write(only_copy(&f.cache), b"garbage").unwrap();
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        assert_eq!(fast(&f.repo, &f.cache), b"?? new.rs\0");
        assert_eq!(markers(&f.cache), 1, "failed copy not marked");
        assert_eq!(assert_matches_plain(&f.repo, &f.cache), b"?? new.rs\0");
    }

    #[test]
    fn test_status_on_private_index_plain_also_fails_writes_no_marker() {
        let Some(f) = served_fixture() else {
            return;
        };
        fast(&f.repo, &f.cache);
        fs::write(f.repo.join(".git/HEAD"), b"not a ref\n").unwrap();
        assert!(
            !plain_out(&f.repo).status.success(),
            "fixture must break git"
        );
        fast(&f.repo, &f.cache);
        assert_eq!(markers(&f.cache), 0, "a refusal plain shares must not mark");
    }

    #[test]
    fn test_status_on_private_index_missing_copy_recreates() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        fast(&f.repo, &f.cache);
        let copy = only_copy(&f.cache);
        fs::remove_file(&copy).unwrap();
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        assert_eq!(fast(&f.repo, &f.cache), b"?? new.rs\0");
        assert_eq!(only_copy(&f.cache), copy);
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
    fn test_porcelain_all_index_shorter_than_checksum_matches_plain_without_copy() {
        for body in [&b""[..], &b"DIRC\0\0\0\x02\0\0"[..]] {
            let f = fixture();
            fs::write(f.repo.join(".git/index"), body).unwrap();
            assert_matches_plain(&f.repo, &f.cache);
            assert!(copies(&f.cache).is_empty(), "{} bytes copied", body.len());
        }
    }

    #[test]
    fn test_porcelain_all_not_a_repo_matches_plain_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(tmp.path()).unwrap();
        let got = porcelain_all_in(&dir, &dir.join("cache"), &isolated_env()).unwrap();
        let expected = plain_out(&dir);
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

    #[test]
    fn test_status_on_private_index_after_run_copy_stamped_between_real_index_and_start() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        fast(&f.repo, &f.cache);
        let finished = SystemTime::now();
        let real = fs::metadata(f.repo.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap();
        let stamp = fs::metadata(only_copy(&f.cache))
            .unwrap()
            .modified()
            .unwrap();
        assert!(
            real <= stamp && stamp <= (finished - Duration::from_secs(1)).max(real),
            "copy stamped {stamp:?}, real index {real:?}, run finished {finished:?}"
        );
    }

    /// The real index trusts `a.txt` by stat, while its content reads as
    /// modified once compared: the blob holds CRLF and a `text` attribute
    /// normalises the worktree file to LF. Whole seconds are set explicitly,
    /// with the entry one second older than the real index.
    #[test]
    fn test_status_on_private_index_crlf_blob_text_attribute_matches_plain() {
        let Some(f) = served_fixture() else {
            return;
        };
        write(&f.repo, "a.txt", "x\r\n");
        git(&f.repo, &["-c", "core.autocrlf=false", "add", "a.txt"]);
        git(&f.repo, &["commit", "-qm", "crlf"]);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let real = UNIX_EPOCH + Duration::from_secs(now.as_secs());
        for (rel, age) in [("lib.rs", 10), ("src/b.rs", 10), ("a.txt", 1)] {
            File::options()
                .write(true)
                .open(f.repo.join(rel))
                .unwrap()
                .set_times(FileTimes::new().set_modified(real - Duration::from_secs(age)))
                .unwrap();
        }
        git(
            &f.repo,
            &[
                "-c",
                "core.autocrlf=false",
                "update-index",
                "-q",
                "--refresh",
            ],
        );
        write(&f.repo, ".git/info/attributes", "*.txt text\n");
        File::options()
            .write(true)
            .open(f.repo.join(".git/index"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(real))
            .unwrap();
        let env = isolated_env();
        let repo = served_repo(&f.repo, &env);
        assert_eq!(serve(&f.repo, &repo, &f.cache), b"");
        assert_eq!(serve(&f.repo, &repo, &f.cache), b"");
    }

    #[test]
    fn test_status_on_private_index_dir_mtime_in_copy_second_lists_new_file() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        fast(&f.repo, &f.cache);
        let stamp = fs::metadata(only_copy(&f.cache))
            .unwrap()
            .modified()
            .unwrap();
        write(&f.repo, "src/new.rs", "pub fn n() {}\n");
        File::open(f.repo.join("src"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(stamp))
            .unwrap();
        assert_eq!(fast(&f.repo, &f.cache), b"?? src/new.rs\0");
    }

    /// A copy at `path` carrying `mtime`.
    fn copy_stamped(path: &Path, mtime: SystemTime) {
        File::create(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(mtime))
            .unwrap();
    }

    fn mtime(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn test_stamp_before_fresh_copy_old_real_index_moves_stamp_back() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("index-x");
        let started = SystemTime::now();
        copy_stamped(&copy, started);
        assert!(stamp_before(
            &copy,
            started,
            started - Duration::from_secs(60)
        ));
        assert_eq!(mtime(&copy), started - Duration::from_secs(1));
    }

    #[test]
    fn test_stamp_before_recent_real_index_stamp_at_real_index() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("index-x");
        let started = SystemTime::now();
        let real = started - Duration::from_millis(300);
        copy_stamped(&copy, started);
        assert!(stamp_before(&copy, started, real));
        assert_eq!(mtime(&copy), real);
    }

    #[test]
    fn test_stamp_before_copy_older_than_real_index_raised_to_real_index() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("index-x");
        let started = SystemTime::now();
        let real = started - Duration::from_secs(30);
        copy_stamped(&copy, real - Duration::from_secs(5));
        assert!(stamp_before(&copy, started, real));
        assert_eq!(mtime(&copy), real);
    }

    #[test]
    fn test_stamp_before_real_index_after_start_false() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("index-x");
        let started = SystemTime::now();
        copy_stamped(&copy, started - Duration::from_secs(5));
        assert!(!stamp_before(
            &copy,
            started,
            started + Duration::from_secs(5)
        ));
    }

    #[test]
    fn test_stamp_before_missing_copy_false() {
        let tmp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert!(!stamp_before(
            &tmp.path().join("gone"),
            now,
            now - Duration::from_secs(5)
        ));
    }

    #[test]
    fn test_porcelain_all_relative_cache_root_other_cwd_matches_plain() {
        let cwd = std::env::current_dir().unwrap();
        let ups = cwd.components().count() - 1;
        // Deeper than the cwd, so the `..` run cannot collapse at `/` and
        // land on the same directory from the worktree as from the cwd.
        let Some(f) = served_fixture_named(&format!("{}repo", "d/".repeat(ups + 1))) else {
            return;
        };
        assert!(
            !cwd.starts_with(&f.repo),
            "cwd must differ from the worktree"
        );
        let mut relative: PathBuf = std::iter::repeat_n("..", ups).collect();
        relative.push(f.cache.strip_prefix("/").unwrap());
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        let env = isolated_env();
        for _ in 0..200 {
            let got = porcelain_all_in(&f.repo, &relative, &env).unwrap();
            assert_eq!(
                got.stdout,
                b"?? new.rs\0",
                "{}",
                String::from_utf8_lossy(&got.stdout)
            );
            if !copies(&f.cache).is_empty() {
                return;
            }
        }
        panic!("the private copy never served");
    }

    #[test]
    fn test_porcelain_all_sparse_checkout_falls_back_through_create_delete() {
        let f = fixture();
        write(&f.repo, "out/file", "x\n");
        git(&f.repo, &["add", "out/file"]);
        git(&f.repo, &["commit", "-qm", "out"]);
        git(&f.repo, &["sparse-checkout", "set", "src"]);
        assert!(!f.repo.join("out/file").exists());
        assert!(assert_matches_plain(&f.repo, &f.cache).is_empty());
        write(&f.repo, "out/file", "x\n");
        assert_matches_plain(&f.repo, &f.cache);
        fs::remove_file(f.repo.join("out/file")).unwrap();
        assert_matches_plain(&f.repo, &f.cache);
        assert!(
            copies(&f.cache).is_empty(),
            "sparse checkout must not be copied"
        );
    }

    fn env_from<const N: usize>(
        pairs: [(&'static str, &'static str); N],
    ) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn test_status_on_private_index_repo_config_change_new_copy_identity() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        fast(&f.repo, &f.cache);
        let before = copies(&f.cache);
        git(&f.repo, &["config", "core.ignorecase", "false"]);
        fast(&f.repo, &f.cache);
        let after = copies(&f.cache);
        assert_eq!(after.len(), 1, "{after:?}");
        assert_ne!(after, before);
    }

    #[test]
    fn test_config_identity_file_change_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let files = [config.clone(), tmp.path().join("absent")];
        fs::write(&config, "[core]\n\tbare = false\n").unwrap();
        let before = config_identity(&files, &[]).unwrap();
        fs::write(&config, "[core]\n\tbare = false\n\tignorecase = true\n").unwrap();
        assert_ne!(config_identity(&files, &[]).unwrap(), before);
    }

    #[test]
    fn test_config_identity_missing_file_appearing_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".gitconfig");
        let files = [global.clone()];
        let absent = config_identity(&files, &[]).expect("absent files are an identity");
        fs::write(&global, "").unwrap();
        assert_ne!(config_identity(&files, &[]).unwrap(), absent);
    }

    #[test]
    fn test_config_identity_unreadable_file_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(config_identity(&[tmp.path().to_path_buf()], &[]), None);
    }

    #[test]
    fn test_config_identity_include_section_none() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        for body in [
            "[include]\n\tpath = other\n",
            "  [includeIf \"gitdir:/x/\"]\n\tpath = other\n",
            "[INCLUDE] path = other\n",
        ] {
            fs::write(&config, body).unwrap();
            assert_eq!(
                config_identity(std::slice::from_ref(&config), &[]),
                None,
                "{body}"
            );
        }
    }

    #[test]
    fn test_blocks_copy_unmodelled_keys_true() {
        for body in [
            "[core]\n\tsparseCheckout = true\n",
            "[core]\n\tsparseCheckoutCone=true\n",
            "[core]\n\ttrustctime = false\n",
            "[core]\n\tcheckStat = minimal\n",
            "[core] worktree = /elsewhere\n",
            "[core]\n\tattributesFile = /elsewhere\n",
        ] {
            assert!(blocks_copy(body.as_bytes()), "{body}");
        }
    }

    #[test]
    fn test_config_identity_attributes_change_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let attributes = [tmp.path().join("info").join("attributes")];
        let absent = config_identity(&[], &attributes).expect("absent attributes are an identity");
        fs::create_dir_all(tmp.path().join("info")).unwrap();
        fs::write(&attributes[0], "*.txt text eol=crlf\n").unwrap();
        let first = config_identity(&[], &attributes).unwrap();
        assert_ne!(first, absent);
        fs::write(&attributes[0], "*.txt -text\n").unwrap();
        assert_ne!(config_identity(&[], &attributes).unwrap(), first);
    }

    #[test]
    fn test_config_identity_attributes_not_screened_as_config() {
        let tmp = tempfile::tempdir().unwrap();
        let attributes = tmp.path().join("attributes");
        fs::write(&attributes, "worktree text\n[include] x\n").unwrap();
        assert!(config_identity(&[], &[attributes]).is_some());
    }

    #[test]
    fn test_attribute_files_names_info_global_and_system_files() {
        let env = env_from([("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]);
        assert_eq!(
            attribute_files(Path::new("/r/.git"), &env).unwrap(),
            [
                "/r/.git/info/attributes",
                "/x/git/attributes",
                "/etc/gitattributes"
            ]
            .map(PathBuf::from)
        );
        let env = env_from([("HOME", "/h")]);
        assert_eq!(
            attribute_files(Path::new("/r/.git"), &env).unwrap()[1],
            PathBuf::from("/h/.config/git/attributes")
        );
    }

    #[test]
    fn test_attribute_files_relative_path_none() {
        let git = Path::new("/r/.git");
        assert_eq!(attribute_files(git, &env_from([("HOME", "h")])), None);
        assert_eq!(
            attribute_files(git, &env_from([("HOME", ""), ("XDG_CONFIG_HOME", "")])),
            None
        );
        assert_eq!(
            attribute_files(git, &env_from([("XDG_CONFIG_HOME", "x"), ("HOME", "/h")])),
            None
        );
    }

    #[test]
    fn test_status_on_private_index_git_runs_with_lock_marked() {
        let Some(f) = served_fixture() else {
            return;
        };
        let env = isolated_env();
        let mut repo = served_repo(&f.repo, &env);
        serve(&f.repo, &repo, &f.cache);
        let lock = only_copy(&f.cache).with_file_name(LOCK_FILE);
        let seen = f.cache.join("seen");
        let wrapper = f.cache.join("git-wrapper");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n/usr/bin/stat -c %s '{}' > '{}'\nexec '{}' \"$@\"\n",
                lock.display(),
                seen.display(),
                repo.git.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        repo.git = wrapper;
        serve(&f.repo, &repo, &f.cache);
        assert_eq!(fs::read_to_string(&seen).unwrap().trim(), "1");
        assert_eq!(fs::metadata(&lock).unwrap().len(), 0);
    }

    #[test]
    fn test_blocks_copy_ordinary_config_false() {
        let body = "[core]\n\tignorecase = false\n[extensions]\n\tworktreeConfig = true\n\
                    [alias]\n\twtnew = \"!git worktree add\"\n# include in a comment\n";
        assert!(!blocks_copy(body.as_bytes()));
    }

    #[test]
    fn test_config_files_distribution_git_reads_etc_gitconfig() {
        let env = env_from([("HOME", "/home/u")]);
        let files = config_files(
            Path::new("/r/.git"),
            Path::new("/r/.git"),
            Path::new("/usr/bin/git"),
            &env,
        )
        .unwrap();
        assert_eq!(
            files,
            [
                "/etc/gitconfig",
                "/r/.git/config",
                "/r/.git/config.worktree",
                "/home/u/.config/git/config",
                "/home/u/.gitconfig",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn test_config_files_other_git_without_system_override_none() {
        let env = env_from([]);
        let files = config_files(
            Path::new("/r/.git"),
            Path::new("/r/.git"),
            Path::new("/opt/git/bin/git"),
            &env,
        );
        assert_eq!(files, None);
    }

    #[test]
    fn test_config_files_env_overrides_named_files() {
        let env = env_from([
            ("GIT_CONFIG_SYSTEM", "/s/gitconfig"),
            ("GIT_CONFIG_GLOBAL", "/g/gitconfig"),
        ]);
        let files = config_files(
            Path::new("/r/.git/worktrees/w"),
            Path::new("/r/.git"),
            Path::new("/opt/git/bin/git"),
            &env,
        )
        .unwrap();
        assert_eq!(
            files,
            [
                "/s/gitconfig",
                "/r/.git/config",
                "/r/.git/worktrees/w/config.worktree",
                "/g/gitconfig",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn test_git_on_path_relative_entry_none() {
        assert_eq!(git_on_path(OsStr::new("bin:/usr/bin")), None);
    }

    #[test]
    fn test_eligible_isolated_env_some() {
        let Some(f) = served_fixture() else {
            return;
        };
        assert!(eligible(&f.repo, &isolated_env()).is_some());
    }

    #[test]
    fn test_eligible_blocking_env_var_none() {
        let Some(f) = served_fixture() else {
            return;
        };
        for blocking in [
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_ATTR_NOSYSTEM",
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_CEILING_DIRECTORIES",
        ] {
            let mut env = isolated_env();
            env.push((blocking.into(), Some("x".into())));
            assert!(eligible(&f.repo, &env).is_none(), "{blocking}");
        }
    }

    #[test]
    fn test_status_on_private_index_path_with_space_and_non_ascii_matches_plain() {
        let Some(f) = served_fixture_named("my repo é") else {
            return;
        };
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        assert_eq!(fast(&f.repo, &f.cache), b"?? new.rs\0");
        assert_eq!(fast(&f.repo, &f.cache), b"?? new.rs\0");
    }

    const MOUNTINFO_SAMPLE: &str = "\
22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
40 22 0:35 / /home/u/repo rw,relatime shared:2 - ext4 /dev/sdb1 rw
41 22 0:36 / /home/u/repo2 rw,relatime shared:3 - fuse.sshfs host: rw
42 22 0:37 / /proc rw,nosuid shared:4 - proc proc rw
";

    #[test]
    fn test_no_mount_below_own_and_unrelated_mounts_true() {
        let info = MOUNTINFO_SAMPLE.as_bytes().to_vec();
        assert!(no_mount_below(Ok(info), Path::new("/home/u/repo")));
    }

    #[test]
    fn test_no_mount_below_mount_inside_worktree_false() {
        let mut info = MOUNTINFO_SAMPLE.as_bytes().to_vec();
        info.extend_from_slice(b"43 40 0:38 / /home/u/repo/vendor rw - fuse.x x rw\n");
        assert!(!no_mount_below(Ok(info), Path::new("/home/u/repo")));
    }

    #[test]
    fn test_no_mount_below_escaped_space_mount_inside_false() {
        let info = b"43 22 0:38 / /home/u/my\\040repo/v\\134x rw - nfs h:/x rw\n".to_vec();
        assert!(!no_mount_below(
            Ok(info.clone()),
            Path::new("/home/u/my repo")
        ));
        assert!(no_mount_below(Ok(info), Path::new("/home/u/my")));
    }

    #[test]
    fn test_no_mount_below_root_worktree_any_other_mount_false() {
        let info = MOUNTINFO_SAMPLE.as_bytes().to_vec();
        assert!(!no_mount_below(Ok(info), Path::new("/")));
    }

    #[test]
    fn test_no_mount_below_unreadable_mountinfo_false() {
        let err = io::Error::from(io::ErrorKind::NotFound);
        assert!(!no_mount_below(Err(err), Path::new("/home/u/repo")));
    }

    #[test]
    fn test_unescape_mount_point_octal_and_literal() {
        assert_eq!(
            unescape_mount_point(b"a\\040b\\011c\\012d\\134e"),
            b"a b\tc\nd\\e"
        );
        assert_eq!(unescape_mount_point(b"a\\9zz\\04"), b"a\\9zz\\04");
    }

    #[test]
    fn test_reliable_magic_local_true_network_false() {
        assert!(reliable_magic(0xEF53));
        assert!(reliable_magic(0x0102_1994));
        assert!(!reliable_magic(0x6969)); // NFS
        assert!(!reliable_magic(0x0102_1997)); // 9p
        assert!(!reliable_magic(0x794C_7630)); // overlay
    }

    #[test]
    fn test_porcelain_all_lock_held_matches_plain_without_copy() {
        let Some(f) = served_fixture() else {
            return;
        };
        write(&f.repo, "new.rs", "pub fn n() {}\n");
        let env = isolated_env();
        let repo = served_repo(&f.repo, &env);
        let dir = copy_dir(&f.cache, repo.gitdir.to_str().unwrap());
        fs::create_dir_all(&dir).unwrap();
        let held = lock_dir(&dir).unwrap();
        assert_eq!(assert_matches_plain(&f.repo, &f.cache), b"?? new.rs\0");
        assert!(
            copies(&f.cache).is_empty(),
            "a held lock must send the run to plain"
        );
        drop(held);
    }

    #[test]
    fn test_sweep_orphans_gone_gitdir_removed_live_and_locked_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(tmp.path()).unwrap();
        let root = home.join(PRIVATE_INDEX_DIR);
        let gone = home.join("deleted-worktree/.git");
        let make = |name: &str, record: Option<&Path>| {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("index-1-2-3-00"), b"copy").unwrap();
            if let Some(gitdir) = record {
                fs::write(dir.join(GITDIR_RECORD), gitdir.to_str().unwrap()).unwrap();
            }
            dir
        };
        let orphan = make("orphan", Some(gone.as_path()));
        let unrecorded = make("unrecorded", None);
        let live = make("live", Some(home.as_path()));
        let locked = make("locked", Some(gone.as_path()));
        let held = lock_dir(&locked).unwrap();

        assert_eq!(sweep_orphans(&home).unwrap(), 2);
        assert!(!orphan.exists());
        assert!(!unrecorded.exists());
        assert!(live.join("index-1-2-3-00").exists());
        assert!(locked.join("index-1-2-3-00").exists());
        drop(held);
        // A sibling test thread's spawn can hold a duplicate of the released
        // lock fd until its exec.
        for _ in 0..200 {
            if sweep_orphans(&home).unwrap() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!locked.exists());
    }

    #[test]
    fn test_sweep_orphans_no_cache_root_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(sweep_orphans(tmp.path()).unwrap(), 0);
    }

    #[test]
    fn test_blocks_copy_byte_order_mark_first_line_true() {
        for body in [
            "\u{feff}[include]\n\tpath = other\n",
            "\u{feff}[core] sparseCheckout = true\n",
            "\u{feff}[extensions]\n\tobjectFormat = sha256\n",
        ] {
            assert!(blocks_copy(body.as_bytes()), "{body:?}");
        }
    }

    #[test]
    fn test_config_files_relative_path_none() {
        for env in [
            env_from([("GIT_CONFIG_SYSTEM", "etc/gitconfig"), ("HOME", "/h")]),
            env_from([("GIT_CONFIG_GLOBAL", "gitconfig"), ("HOME", "/h")]),
            env_from([("HOME", "h"), ("XDG_CONFIG_HOME", "/x")]),
            env_from([("HOME", ""), ("XDG_CONFIG_HOME", "/x")]),
            env_from([("XDG_CONFIG_HOME", "x"), ("HOME", "/h")]),
        ] {
            let files = config_files(
                Path::new("/r/.git"),
                Path::new("/r/.git"),
                Path::new("/usr/bin/git"),
                &env,
            );
            assert_eq!(files, None);
        }
    }

    #[test]
    fn test_status_on_private_index_config_changed_during_run_falls_back() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        let env = isolated_env();
        let repo = served_repo(&f.repo, &env);
        git(&f.repo, &["config", "core.ignorecase", "false"]);
        assert!(status_on_private_index(&f.repo, &repo, &f.cache).is_none());
        assert!(copies(&f.cache).is_empty());
    }

    /// The copy holds an index without `src/b.rs`, as a run killed between
    /// git's rewrite and the stamp could leave a stale one; trusting it lists
    /// the file as deleted and untracked.
    #[test]
    fn test_status_on_private_index_marked_lock_rebuilds_copy() {
        let Some(f) = served_fixture() else {
            return;
        };
        git(&f.repo, &["rm", "-q", "--cached", "src/b.rs"]);
        let stale = fs::read(f.repo.join(".git/index")).unwrap();
        git(&f.repo, &["reset", "-q"]);
        settle(&f.repo);
        let env = isolated_env();
        let repo = served_repo(&f.repo, &env);
        serve(&f.repo, &repo, &f.cache);
        let copy = only_copy(&f.cache);
        fs::write(&copy, stale).unwrap();
        let lock = copy.with_file_name(LOCK_FILE);
        File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert_eq!(serve(&f.repo, &repo, &f.cache), b"");
        assert_eq!(fs::metadata(&lock).unwrap().len(), 0);
    }

    /// Requires `worktree` to take the plain command and leave no copy.
    fn assert_falls_back(worktree: &Path, cache: &Path) {
        let env = isolated_env();
        let repo = served_repo(worktree, &env);
        assert!(status_on_private_index(worktree, &repo, cache).is_none());
        assert!(copies(cache).is_empty());
    }

    #[test]
    fn test_status_on_private_index_submodule_falls_back() {
        let Some(f) = served_fixture() else {
            return;
        };
        let source = fixture_named("source");
        git(
            &f.repo,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                source.repo.to_str().unwrap(),
                "sub",
            ],
        );
        git(&f.repo, &["commit", "-qm", "sub"]);
        assert!(f.repo.join(".gitmodules").is_file());
        assert!(f.repo.join(".git/modules").is_dir());
        let sub = f.repo.join("sub");
        git(&sub, &["config", "status.showUntrackedFiles", "no"]);
        write(&sub, "untracked.rs", "pub fn u() {}\n");
        assert_eq!(plain_out(&f.repo).stdout, b"");
        assert_falls_back(&f.repo, &f.cache);
    }

    #[test]
    fn test_status_on_private_index_embedded_repo_gitlink_falls_back() {
        let Some(f) = served_fixture() else {
            return;
        };
        let inner = f.repo.join("inner");
        fs::create_dir(&inner).unwrap();
        git(&inner, &["init", "-q", "-b", "main"]);
        write(&inner, "f.rs", "pub fn f() {}\n");
        git(&inner, &["add", "f.rs"]);
        git(&inner, &["commit", "-qm", "inner"]);
        git(&inner, &["config", "status.showUntrackedFiles", "no"]);
        git(&f.repo, &["add", "inner"]);
        git(&f.repo, &["commit", "-qm", "gitlink"]);
        assert!(!f.repo.join(".gitmodules").exists());
        write(&inner, "untracked.rs", "pub fn u() {}\n");
        assert_eq!(plain_out(&f.repo).stdout, b"");
        assert_falls_back(&f.repo, &f.cache);
    }

    #[test]
    fn test_gitlink_free_index_versions_walk_every_entry() {
        let f = fixture();
        write(&f.repo, "src/deeper/intent.rs", "pub fn i() {}\n");
        git(&f.repo, &["add", "-N", "src/deeper/intent.rs"]);
        let index = f.repo.join(".git/index");
        for version in ["2", "3", "4"] {
            git(&f.repo, &["update-index", "--index-version", version]);
            assert!(gitlink_free(&fs::read(&index).unwrap()), "v{version}");
        }
        let inner = f.repo.join("zz/inner");
        fs::create_dir_all(&inner).unwrap();
        git(&inner, &["init", "-q", "-b", "main"]);
        write(&inner, "f.rs", "pub fn f() {}\n");
        git(&inner, &["add", "f.rs"]);
        git(&inner, &["commit", "-qm", "inner"]);
        git(&f.repo, &["add", "zz/inner"]);
        for version in ["2", "3", "4"] {
            git(&f.repo, &["update-index", "--index-version", version]);
            assert!(!gitlink_free(&fs::read(&index).unwrap()), "v{version}");
        }
        assert!(!gitlink_free(b"DIRC\0\0\0\x02\0\0\0\x01"));
    }

    #[test]
    fn test_status_on_private_index_cache_inside_worktree_falls_back() {
        let Some(f) = served_fixture() else {
            return;
        };
        settle(&f.repo);
        let cache = f.repo.join("ecp-cache/git-index");
        assert_falls_back(&f.repo, &cache);
        assert_eq!(plain_out(&f.repo).stdout, b"");
    }

    #[test]
    fn test_lock_at_path_replaced_lock_file_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        let open = || {
            File::options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let old = open();
        fs::rename(&path, tmp.path().join("retired")).unwrap();
        let fresh = open();
        assert!(lock_at_path(old, &path).is_none());
        assert!(lock_at_path(fresh, &path).is_some());
    }

    /// An entry gc cannot delete stops `remove_dir_all` part way. Deleting in
    /// place would leave the directory, its lock gone, where the next query
    /// locks a fresh file and shares it with gc.
    #[test]
    fn test_sweep_orphans_undeletable_entry_leaves_no_dir_at_live_path() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(tmp.path()).unwrap();
        let root = home.join(PRIVATE_INDEX_DIR);
        let dir = root.join("0123456789abcdef");
        let stuck = dir.join("stuck");
        fs::create_dir_all(&stuck).unwrap();
        fs::write(stuck.join("f"), b"x").unwrap();
        let gone = home.join("gone/.git");
        fs::write(dir.join(GITDIR_RECORD), gone.to_str().unwrap()).unwrap();
        let chmod = |path: &Path, mode| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        };
        chmod(&stuck, 0o500);
        if File::create(stuck.join("probe")).is_ok() {
            chmod(&stuck, 0o700);
            eprintln!("skipped: directory permissions do not bind this user");
            return;
        }
        sweep_orphans(&home).unwrap();
        let left_in_place = dir.exists();
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let leftover = entry.path().join("stuck");
            if leftover.exists() {
                chmod(&leftover, 0o700);
            }
        }
        assert!(!left_in_place, "gc left a half-deleted directory in place");
    }
}
